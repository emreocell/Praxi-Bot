//! Hashing, time and file-tree helpers shared by the hub.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::HubError;

/// Most files a skill may contain.
pub const MAX_FILES: usize = 500;
/// Most total bytes a skill may contain.
pub const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
/// Deepest directory nesting accepted.
const MAX_DEPTH: usize = 16;

/// One regular file of a skill tree.
#[derive(Debug, Clone)]
pub struct TreeFile {
    /// Path relative to the skill root, `/`-separated.
    pub rel: String,
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 of `bytes` as lowercase hex.
#[allow(dead_code)] // Kept for future debugging/inspection tools
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Content hash over `(relative path, length, bytes)` of `files`, in order.
pub fn hash_files(files: &[TreeFile]) -> Result<String, HubError> {
    let mut h = Sha256::new();
    for f in files {
        let bytes = fs::read(&f.path).map_err(|e| io_err("read", &f.path, &e))?;
        h.update(f.rel.as_bytes());
        h.update([0]);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    Ok(hex(&h.finalize()))
}

/// Wrap an I/O error with the failing operation and path.
pub fn io_err(op: &str, path: &Path, e: &std::io::Error) -> HubError {
    HubError::Io(format!("{op} {}: {e}", path.display()))
}

/// Collect the regular files below `root`, sorted by relative path. `.git`
/// directories are skipped.
///
/// # Errors
/// Symlinks, other special files, and trees exceeding the limits are refused.
pub fn walk(root: &Path) -> Result<Vec<TreeFile>, HubError> {
    let mut out = Vec::new();
    let mut total = 0u64;
    walk_inner(root, "", 0, &mut out, &mut total)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk_inner(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<TreeFile>,
    total: &mut u64,
) -> Result<(), HubError> {
    if depth > MAX_DEPTH {
        return Err(HubError::TooLarge("directory nesting is too deep".into()));
    }
    let entries = fs::read_dir(dir).map_err(|e| io_err("read dir", dir, &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err("read dir", dir, &e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| io_err("stat", &path, &e))?;
        let ft = meta.file_type();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if ft.is_symlink() {
            return Err(HubError::Unsafe(format!("symlink `{rel}` is not allowed")));
        }
        if ft.is_dir() {
            if name == ".git" {
                continue;
            }
            walk_inner(&path, &rel, depth + 1, out, total)?;
        } else if ft.is_file() {
            *total += meta.len();
            if out.len() >= MAX_FILES || *total > MAX_TOTAL_BYTES {
                return Err(HubError::TooLarge(format!(
                    "skill exceeds {MAX_FILES} files or {MAX_TOTAL_BYTES} bytes"
                )));
            }
            out.push(TreeFile {
                rel,
                path,
                size: meta.len(),
            });
        } else {
            return Err(HubError::Unsafe(format!(
                "special file `{rel}` is not allowed"
            )));
        }
    }
    Ok(())
}

/// Copy `files` (relative to the tree they came from) into `dest`.
pub fn copy_files(files: &[TreeFile], dest: &Path) -> Result<(), HubError> {
    for f in files {
        let target = dest.join(&f.rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err("create dir", parent, &e))?;
        }
        fs::copy(&f.path, &target).map_err(|e| io_err("copy", &f.path, &e))?;
    }
    Ok(())
}

/// Write `data` to `path` atomically (temp file in the same directory + rename).
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), HubError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create dir", parent, &e))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data).map_err(|e| io_err("write", &tmp, &e))?;
    fs::rename(&tmp, path).map_err(|e| io_err("rename", path, &e))
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339_from_secs(secs)
}

/// Format Unix seconds as RFC 3339 UTC.
pub fn rfc3339_from_secs(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
