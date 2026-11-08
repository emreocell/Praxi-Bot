//! Hashing, time and file-tree helpers shared by the hub.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
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
    refuse_reparse(root)?;
    let mut out = Vec::new();
    let mut total = 0u64;
    walk_inner(root, "", 0, &mut out, &mut total)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    ensure_no_alternate_streams(root)?;
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
        if ft.is_symlink() || is_reparse_point(&meta) {
            return Err(HubError::Unsafe(format!(
                "reparse point `{rel}` is not allowed"
            )));
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

/// Copy only the default data stream of `files` into `dest`.
///
/// `fs::copy` uses `CopyFile` on Windows, which also copies NTFS alternate
/// data streams that the scanner never read. Reading and writing the default
/// stream leaves those streams behind.
pub fn copy_files(files: &[TreeFile], dest: &Path) -> Result<(), HubError> {
    for f in files {
        refuse_reparse(&f.path)?;
        if !relative_stays_inside(&f.rel) {
            return Err(HubError::Unsafe(format!(
                "refusing to copy unsafe relative path `{}`",
                f.rel
            )));
        }
        let target = dest.join(&f.rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err("create dir", parent, &e))?;
        }
        copy_default_stream(&f.path, &target)?;
        refuse_reparse(&target)?;
    }
    Ok(())
}

fn relative_stays_inside(rel: &str) -> bool {
    let path = Path::new(rel);
    !rel.is_empty()
        && !path.is_absolute()
        && !rel.starts_with('/')
        && !rel.starts_with('\\')
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn copy_default_stream(src: &Path, dest: &Path) -> Result<(), HubError> {
    let mut input = fs::File::open(src).map_err(|e| io_err("open", src, &e))?;
    let mut output = fs::File::create(dest).map_err(|e| io_err("create", dest, &e))?;
    std::io::copy(&mut input, &mut output).map_err(|e| io_err("copy", src, &e))?;
    Ok(())
}

fn is_reparse_point(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Refuse symlinks, junctions, and other reparse points.
///
/// # Errors
/// [`HubError::Unsafe`] when `path` is a reparse point, or [`HubError::Io`]
/// when the path cannot be stat'd.
pub fn refuse_reparse(path: &Path) -> Result<(), HubError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || is_reparse_point(&meta) => Err(
            HubError::Unsafe(format!("reparse point `{}` is not allowed", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err("stat", path, &e)),
    }
}

fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// True when `dir` is exactly one normal component inside `root`.
fn is_strict_child(root: &Path, dir: &Path) -> bool {
    let mut rest = dir.components();
    for component in root.components() {
        match rest.next() {
            Some(next) if component_eq(component, next) => {}
            _ => return false,
        }
    }
    matches!(
        (rest.next(), rest.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// `dir` may be deleted only when it is a direct child of `root`, is not a
/// reparse point, and its canonical path stays inside `root`.
///
/// # Errors
/// [`HubError::Unsafe`] when the path escapes or is a reparse point.
pub fn ensure_deletable(root: &Path, dir: &Path) -> Result<(), HubError> {
    let Some(parent) = dir.parent() else {
        return Err(HubError::Unsafe(
            "refusing to delete a path outside the skills directory".into(),
        ));
    };
    if parent != root {
        return Err(HubError::Unsafe(format!(
            "refusing to delete `{}` outside the skills directory",
            dir.display()
        )));
    }
    refuse_reparse(root)?;
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err("stat", dir, &e)),
        Ok(meta) => {
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                return Err(HubError::Unsafe(format!(
                    "reparse point `{}` is not allowed",
                    dir.display()
                )));
            }
            let root_canon =
                fs::canonicalize(root).map_err(|e| io_err("canonicalize", root, &e))?;
            let dir_canon = fs::canonicalize(dir).map_err(|e| io_err("canonicalize", dir, &e))?;
            if !is_strict_child(&root_canon, &dir_canon) {
                return Err(HubError::Unsafe(format!(
                    "refusing to delete `{}` outside the skills directory",
                    dir.display()
                )));
            }
            Ok(())
        }
    }
}

/// Delete `dir` after [`ensure_deletable`] succeeds. A missing directory is
/// not an error.
///
/// # Errors
/// [`HubError::Unsafe`] when the path is outside `root` or is a reparse point.
pub fn remove_dir_inside(root: &Path, dir: &Path) -> Result<(), HubError> {
    ensure_deletable(root, dir)?;
    if fs::symlink_metadata(dir).is_ok() {
        ensure_deletable(root, dir)?;
        fs::remove_dir_all(dir).map_err(|e| io_err("remove", dir, &e))?;
    }
    Ok(())
}

fn ensure_no_alternate_streams(root: &Path) -> Result<(), HubError> {
    #[cfg(windows)]
    {
        reject_windows_alternate_streams(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Ok(())
    }
}

#[cfg(windows)]
fn reject_windows_alternate_streams(root: &Path) -> Result<(), HubError> {
    use std::process::Command;
    let encoded = powershell_encoded(STREAM_SCRIPT);
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded,
        ])
        .env("PRAXI_HUB_ROOT", root)
        .output()
        .map_err(|e| HubError::Unsafe(format!("cannot inspect alternate data streams: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let hit = out.status.code() == Some(3) || !stdout.trim().is_empty();
    if out.status.success() && stdout.trim().is_empty() {
        return Ok(());
    }
    if hit {
        let detail = stdout.lines().next().unwrap_or("").trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(HubError::Unsafe(format!(
            "alternate data stream is not allowed{suffix}"
        )));
    }
    Err(HubError::Unsafe(format!(
        "cannot inspect alternate data streams: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

#[cfg(windows)]
fn powershell_encoded(script: &str) -> String {
    use base64::Engine;
    let wide: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(wide)
}

#[cfg(windows)]
const STREAM_SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = $env:PRAXI_HUB_ROOT
if ([string]::IsNullOrEmpty($root)) { exit 2 }
$hit = $false
$items = New-Object System.Collections.Generic.List[string]
[void]$items.Add($root)
Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction Stop | ForEach-Object {
  if (-not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    [void]$items.Add($_.FullName)
  }
}
foreach ($path in $items) {
  $streams = @(Get-Item -LiteralPath $path -Force -Stream * -ErrorAction SilentlyContinue)
  foreach ($s in $streams) {
    if ($s.Stream -ne ':$DATA' -and $s.Stream -ne '$DATA') {
      $hit = $true
      [Console]::Out.WriteLine($path)
      break
    }
  }
}
if ($hit) { exit 3 }
exit 0
";

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
