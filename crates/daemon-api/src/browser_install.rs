//! "Install browser": a pinned, checksum-verified Chrome for Testing in the data directory.
//!
//! For a Linux computer that has no usable browser (a server, or Ubuntu where the only Chromium is
//! the snap one, which cannot run under the hardened service). Run by the doctor repair
//! `install_browser` (Settings > Diagnostics) and by `bot browser install`; never on its own.
//!
//! The archive comes from Google's Chrome for Testing storage over https, pinned by version, size
//! and SHA-256 (the same pin `scripts/install-linux.sh` uses; a test keeps the two equal). It goes
//! through the same fail-closed path as the managed `llama-server`: the bytes are hashed and
//! checked before anything is unpacked, unpacking refuses unsafe paths, and the folder gets a
//! manifest of every file's hash, written last. `browser_cdp::launch::find_browser` finds the
//! result after the system browsers, and starts it only when the manifest still matches every
//! file ([`register`] hands it that check); a marker file alone never makes a folder a browser.
//!
//! What the unpacked folder is guaranteed to be (checked by `harden_install` on top of what the
//! shared unpacker already refuses, and tested here): no path with `..` or an absolute path, no
//! symlink or hard link, total size within `UNPACKED_FACTOR` times the archive, no setuid or
//! setgid or sticky bit, files `0o755` or `0o644`, folders `0o755`. A folder that breaks one of
//! these is deleted and the install fails.
//!
//! Chrome for Testing does not update itself: a newer pin ships with a newer Praxi, and the button
//! is offered again only when no browser works.
//!
//! What this cannot do, on purpose: install system libraries or an `AppArmor` profile (both need
//! root). When the browser starts badly because of those, the error says which.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use browser_cdp::launch::{MANAGED_DIR, MANAGED_MARKER, managed_browser};
use provider_catalog::local_runtime::{
    ArchiveKind, DownloadError, DownloadSpec, TrustError, download, install_verified,
    verified_binary, verify_install,
};

/// Chrome for Testing version of the pins below (kept equal to `scripts/install-linux.sh`).
pub const CFT_VERSION: &str = "157.0.8103.38";

/// One downloadable build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserAsset {
    /// Chrome for Testing platform name (`linux64`, `linux-arm64`).
    pub platform: &'static str,
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the whole zip.
    pub sha256: &'static str,
    pub size_bytes: u64,
}

const LINUX_X64: BrowserAsset = BrowserAsset {
    platform: "linux64",
    url: "https://storage.googleapis.com/chrome-for-testing-public/157.0.8103.38/linux64/chrome-linux64.zip",
    sha256: "8cdeb344e9b2cefe59f4aea8d1d93d5d93054c03cde217665c16fc1f62e72a66",
    size_bytes: 203_502_190,
};
const LINUX_ARM64: BrowserAsset = BrowserAsset {
    platform: "linux-arm64",
    url: "https://storage.googleapis.com/chrome-for-testing-public/157.0.8103.38/linux-arm64/chrome-linux-arm64.zip",
    sha256: "5517510e61657ff43ee9ab804ecd1a499b3d5bd3caf72f097516bd6565ba5a17",
    size_bytes: 203_276_965,
};

/// The build for this computer (`std::env::consts` spelling), or `None`: only Linux gets an
/// automatic install (Windows and macOS have Edge or Chrome in the usual places).
#[must_use]
pub fn asset_for(os: &str, arch: &str) -> Option<BrowserAsset> {
    match (os, arch) {
        ("linux", "x86_64") => Some(LINUX_X64),
        ("linux", "aarch64") => Some(LINUX_ARM64),
        _ => None,
    }
}

/// Whether the one-click install makes sense here: a build exists, and `BOT_BROWSER_PATH` is not
/// set (an override always wins, so installing another browser would change nothing).
#[must_use]
pub fn offered(os: &str, arch: &str, override_set: bool) -> bool {
    !override_set && asset_for(os, arch).is_some()
}

/// Where the managed browsers live.
#[must_use]
pub fn root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(MANAGED_DIR)
}

/// Why the install did not finish. `Display` is plain language for the owner.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// No pinned build for this operating system or CPU.
    Unsupported {
        os: String,
        arch: String,
    },
    /// Another install is already running.
    Busy,
    NotEnoughDisk {
        need_bytes: u64,
        free_bytes: u64,
    },
    Network(String),
    /// The download or the unpacked files did not match the pinned checksum; nothing was kept.
    Checksum,
    Unpack(String),
    Io(String),
    /// Downloaded and verified, but the browser does not start on this computer.
    CannotStart {
        binary: PathBuf,
        reason: String,
    },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { os, arch } => write!(
                f,
                "there is no ready-made browser for {os} on {arch}. Install Google Chrome or Chromium yourself and set BOT_BROWSER_PATH"
            ),
            Self::Busy => write!(f, "the browser is already being installed"),
            Self::NotEnoughDisk {
                need_bytes,
                free_bytes,
            } => write!(
                f,
                "not enough free disk space for the browser: about {} MB needed, {} MB free",
                need_bytes.div_ceil(1_000_000),
                free_bytes / 1_000_000
            ),
            Self::Network(e) => write!(
                f,
                "could not download the browser from storage.googleapis.com ({e}). Check the internet connection and the firewall, then try again"
            ),
            Self::Checksum => write!(
                f,
                "the downloaded browser did not match its checksum, so it was thrown away and nothing was installed. Try again; if it keeps failing, report it"
            ),
            Self::Unpack(e) => write!(f, "could not unpack the browser: {e}"),
            Self::Io(e) => write!(f, "could not write the browser files: {e}"),
            Self::CannotStart { binary, reason } => write!(
                f,
                "the browser was downloaded to {} but cannot start on this computer: {reason}",
                binary.display()
            ),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<DownloadError> for InstallError {
    fn from(e: DownloadError) -> Self {
        match e {
            DownloadError::NotEnoughDisk {
                need_bytes,
                free_bytes,
            } => Self::NotEnoughDisk {
                need_bytes,
                free_bytes,
            },
            DownloadError::ChecksumMismatch | DownloadError::TooLarge => Self::Checksum,
            DownloadError::Cancelled => Self::Network("stopped".into()),
            DownloadError::Incomplete => {
                Self::Network("the download ended early; run it again to continue".into())
            }
            DownloadError::InsecureUrl => Self::Network("only https downloads are allowed".into()),
            DownloadError::Network(m) => Self::Network(m),
            DownloadError::Io(m) => Self::Io(m),
        }
    }
}

impl From<TrustError> for InstallError {
    fn from(e: TrustError) -> Self {
        match e {
            TrustError::ArchiveMismatch => Self::Checksum,
            TrustError::Untrusted => Self::Unpack("the archive does not hold a browser".into()),
            TrustError::Extract(m) => Self::Unpack(m),
            TrustError::Io(m) => Self::Io(m),
        }
    }
}

/// The two things that touch the outside world, so tests can fake both.
pub trait Environment: Send + Sync {
    /// Download `asset` to `dest` (`progress` gets the bytes on disk so far). Must check the
    /// size and SHA-256 itself or leave that to [`install`], which re-checks before unpacking.
    fn fetch(
        &self,
        asset: &BrowserAsset,
        dest: &Path,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> impl Future<Output = Result<(), DownloadError>> + Send;

    /// Start the browser once and stop it again: `Err(why)` when it cannot start here.
    fn start_check(&self, binary: &Path) -> impl Future<Output = Result<(), String>> + Send;
}

/// The real network and a real launch.
pub struct System;

impl Environment for System {
    async fn fetch(
        &self,
        asset: &BrowserAsset,
        dest: &Path,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<(), DownloadError> {
        let spec = DownloadSpec {
            url: asset.url,
            dest,
            sha256: asset.sha256,
            size_bytes: asset.size_bytes,
        };
        download(&spec, progress, &AtomicBool::new(false)).await
    }

    async fn start_check(&self, binary: &Path) -> Result<(), String> {
        let opts = browser_cdp::LaunchOptions {
            executable: Some(binary.to_owned()),
            startup_timeout: std::time::Duration::from_secs(20),
            ..browser_cdp::LaunchOptions::default()
        };
        let browser = browser_cdp::Browser::launch(opts)
            .await
            .map_err(|e| with_missing_library_note(&e.to_string()))?;
        let _ = browser.close().await;
        Ok(())
    }
}

/// Libraries Chrome for Testing links against on Debian and Ubuntu (a test keeps this equal to
/// `install_cft` in `scripts/install-linux.sh`, which installs them with root).
const CHROME_LIBRARIES: [&str; 13] = [
    "libnss3",
    "libatk1.0-0",
    "libatk-bridge2.0-0",
    "libcups2",
    "libdrm2",
    "libxkbcommon0",
    "libxcomposite1",
    "libxdamage1",
    "libxfixes3",
    "libxrandr2",
    "libgbm1",
    "libpango-1.0-0",
    "libcairo2",
];

/// Libraries that got a `t64` suffix on Ubuntu 24.04 and Debian 13. There the old name is only a
/// virtual package, and `apt-get install libasound2` refuses it.
const CHROME_LIBRARIES_T64: [&str; 2] = ["libasound2", "libatspi2.0-0"];

/// The `apt-get install` command for the libraries Chrome links against on Debian and Ubuntu.
#[must_use]
pub fn apt_install_command() -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    apt_install_command_for(&os_release)
}

fn apt_install_command_for(os_release: &str) -> String {
    let suffix = if has_t64_libraries(os_release) {
        "t64"
    } else {
        ""
    };
    let packages = CHROME_LIBRARIES
        .iter()
        .map(|p| (*p).to_owned())
        .chain(CHROME_LIBRARIES_T64.iter().map(|p| format!("{p}{suffix}")))
        .collect::<Vec<_>>();
    format!("sudo apt-get install -y {}", packages.join(" "))
}

/// Ubuntu 24.04 and Debian 13 and later carry the `t64` names (the installer script picks them the
/// same way). Unknown systems keep the old names.
fn has_t64_libraries(os_release: &str) -> bool {
    let field = |key: &str| {
        os_release
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"'))
    };
    let major = field("VERSION_ID").and_then(|v| v.split('.').next()?.parse::<u32>().ok());
    matches!(
        (field("ID"), major),
        (Some("ubuntu"), Some(24..)) | (Some("debian"), Some(13..) | None)
    )
}

/// A browser that cannot load a shared library is the commonest failure on a bare server image:
/// say which package family to install (the installer script does this with root).
fn with_missing_library_note(message: &str) -> String {
    if message.contains("missing system libraries")
        || message.to_ascii_lowercase().contains("shared libraries")
    {
        format!(
            "{message}. Install the libraries Chrome needs (on Ubuntu or Debian: {}), or run `sudo ./install-linux.sh --update --with-desktop`",
            apt_install_command()
        )
    } else {
        message.to_owned()
    }
}

/// Data directories with an install under way: one at a time per directory.
static RUNNING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub(crate) struct Running(PathBuf);

impl Running {
    pub(crate) fn start(data_dir: &Path) -> Result<Self, InstallError> {
        let mut r = RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if r.iter().any(|p| p == data_dir) {
            return Err(InstallError::Busy);
        }
        r.push(data_dir.to_owned());
        Ok(Self(data_dir.to_owned()))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|p| *p != self.0);
    }
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub binary: PathBuf,
    /// A verified copy was already there; nothing was downloaded.
    pub already_there: bool,
}

/// Disk the unpacked browser takes, in archive sizes (about 2.1 in practice). Also the most an
/// unpacked archive may add up to: a bigger one is refused and removed.
const UNPACKED_FACTOR: u64 = 3;
/// The download may take this long in all (200 MB on a poor link); it resumes on the next try.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20 * 60);
/// Starting the browser once to prove it runs.
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long each network or launch step of an install may take.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub fetch: std::time::Duration,
    pub start: std::time::Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fetch: FETCH_TIMEOUT,
            start: START_TIMEOUT,
        }
    }
}
/// Free space left over after the install.
const DISK_MARGIN: u64 = 512 * 1024 * 1024;

/// Install (or re-check) the browser under `<data_dir>/browser`.
///
/// A copy that is already there and passes its manifest is reused. Otherwise the archive is
/// downloaded (resumable), its SHA-256 checked, unpacked into a fresh folder with a manifest, the
/// older installs and the archive are removed, and the browser is started once to prove it runs.
///
/// # Errors
/// See [`InstallError`]. After [`InstallError::CannotStart`] the files stay (they are good; the
/// computer lacks something).
pub async fn install(
    data_dir: &Path,
    os: &str,
    arch: &str,
    env: &impl Environment,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<Installed, InstallError> {
    let asset = asset_for(os, arch).ok_or_else(|| InstallError::Unsupported {
        os: os.to_owned(),
        arch: arch.to_owned(),
    })?;
    install_asset(data_dir, asset, env, progress).await
}

/// [`install`] for an explicit pin (the tests use a tiny archive).
///
/// # Errors
/// See [`InstallError`].
pub async fn install_asset(
    data_dir: &Path,
    asset: BrowserAsset,
    env: &impl Environment,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<Installed, InstallError> {
    install_limited(data_dir, asset, env, progress, Limits::default()).await
}

pub(crate) async fn install_limited(
    data_dir: &Path,
    asset: BrowserAsset,
    env: &impl Environment,
    progress: &(dyn Fn(u64) + Send + Sync),
    limits: Limits,
) -> Result<Installed, InstallError> {
    let _running = Running::start(data_dir)?;
    let root = root_dir(data_dir);
    std::fs::create_dir_all(&root).map_err(|e| InstallError::Io(e.kind().to_string()))?;
    restrict_dir(&root);

    // Only a copy that passes its manifest counts; a bare file is never trusted.
    let existing = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || verified_binary(&root, "chrome"))
            .await
            .map_err(|e| InstallError::Io(e.to_string()))?
    };
    let (binary, already_there) = match existing {
        Some(b) => (b, true),
        None => (
            fetch_and_unpack(&root, asset, env, progress, limits.fetch).await?,
            false,
        ),
    };
    let started = tokio::time::timeout(limits.start, env.start_check(&binary))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "it did not start within {} seconds",
                limits.start.as_secs()
            ))
        });
    started.map_err(|reason| InstallError::CannotStart {
        binary: binary.clone(),
        reason,
    })?;
    Ok(Installed {
        binary,
        already_there,
    })
}

async fn fetch_and_unpack(
    root: &Path,
    asset: BrowserAsset,
    env: &impl Environment,
    progress: &(dyn Fn(u64) + Send + Sync),
    fetch_timeout: std::time::Duration,
) -> Result<PathBuf, InstallError> {
    let need = asset.size_bytes * UNPACKED_FACTOR + DISK_MARGIN;
    if let Ok(free) = fs4::available_space(root)
        && free < need
    {
        return Err(InstallError::NotEnoughDisk {
            need_bytes: need,
            free_bytes: free,
        });
    }
    let downloads = root.join("downloads");
    let archive = downloads.join(format!(
        "chrome-for-testing-{CFT_VERSION}-{}.zip",
        asset.platform
    ));
    // A stalled connection must not hold the one install slot for ever; the partial file stays,
    // so the next try continues where this one stopped.
    tokio::time::timeout(fetch_timeout, env.fetch(&asset, &archive, progress))
        .await
        .map_err(|_| {
            InstallError::Network(format!(
                "the download took longer than {} minutes; run it again to continue",
                fetch_timeout.as_secs().div_ceil(60)
            ))
        })??;

    let (a, r, sha) = (archive.clone(), root.to_owned(), asset.sha256);
    let cap = asset.size_bytes.saturating_mul(UNPACKED_FACTOR);
    let outcome = tokio::task::spawn_blocking(move || -> Result<PathBuf, InstallError> {
        // `install_verified` hashes the very file it then unpacks, so a download that was
        // swapped after `fetch` (or a fake that skipped the check) is still refused.
        let binary = install_verified(ArchiveKind::Zip, &a, sha, &r, "chrome")?;
        harden_install(&binary, &r, cap)?;
        Ok(binary)
    })
    .await
    .map_err(|e| InstallError::Io(e.to_string()))?;
    let binary = match outcome {
        Ok(b) => b,
        Err(e) => {
            // A bad archive is thrown away, never kept for another try.
            if e == InstallError::Checksum {
                let _ = std::fs::remove_file(&archive);
            }
            return Err(e);
        }
    };

    // Done: the archive and every older install are no longer needed.
    let _ = std::fs::remove_dir_all(&downloads);
    remove_older_installs(root, &binary);
    Ok(binary)
}

/// Delete the install folders next to the one `keep` lives in (`i-*` folders with a manifest).
fn remove_older_installs(root: &Path, keep: &Path) {
    let Some(keep_dir) = keep
        .ancestors()
        .find(|a| a.parent() == Some(root) && a.join(MANAGED_MARKER).is_file())
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p != keep_dir
            && e.file_type().is_ok_and(|t| t.is_dir())
            && e.file_name().to_string_lossy().starts_with("i-")
        {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The unpacked install folder that holds `binary` (a direct child of `root`), made safe or
/// deleted.
///
/// Walks every entry without following links and refuses: a symlink, a hard link (a file with
/// more than one name could be an alias of something outside), more than `max_bytes` in all, or
/// anything that is not a plain file or folder. Then sets sane modes: setuid, setgid and sticky
/// bits and every group/other write bit go; a file with an execute bit becomes `0o755`, any other
/// file `0o644`, a folder `0o755` (the install folder itself stays `0o700`). On a refusal the
/// whole install folder is removed.
///
/// # Errors
/// [`InstallError::Unpack`] naming what was wrong.
pub(crate) fn harden_install(
    binary: &Path,
    root: &Path,
    max_bytes: u64,
) -> Result<(), InstallError> {
    let Some(install_dir) = binary.ancestors().find(|a| a.parent() == Some(root)) else {
        return Err(InstallError::Unpack(
            "the program is not inside the install folder".into(),
        ));
    };
    let result = harden_tree(install_dir, max_bytes);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(install_dir);
    }
    result.map_err(InstallError::Unpack)
}

fn harden_tree(top: &Path, max_bytes: u64) -> Result<(), String> {
    let mut total = 0u64;
    let mut stack = vec![top.to_path_buf()];
    while let Some(dir) = stack.pop() {
        set_mode(&dir, true).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.kind().to_string())?.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.kind().to_string())?;
            let shown = path
                .strip_prefix(top)
                .unwrap_or(&path)
                .display()
                .to_string();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                if link_count(&meta) > 1 {
                    return Err(format!("{shown} is a hard link"));
                }
                total = total.saturating_add(meta.len());
                if total > max_bytes {
                    return Err(format!(
                        "the archive unpacks to more than the {} MB expected",
                        max_bytes / 1_000_000
                    ));
                }
                set_mode(&path, false).map_err(|e| format!("{shown}: {e}"))?;
            } else {
                return Err(format!("{shown} is a link or a special file"));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn link_count(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(not(unix))]
fn link_count(_: &std::fs::Metadata) -> u64 {
    1
}

// On Windows there is no mode to set, so the body cannot fail; the signature is shared with Unix.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn set_mode(path: &Path, dir: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = std::fs::symlink_metadata(path)?.mode();
        let want = if dir {
            (mode & 0o755) | 0o700
        } else if mode & 0o100 != 0 {
            0o755
        } else {
            0o644
        };
        if mode & 0o7777 != want {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(want))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, dir);
    Ok(())
}

/// The check browser discovery runs before it starts a managed browser: every file of the
/// browser's install folder must still be exactly what the install recorded in its manifest.
#[must_use]
pub fn verify_managed(binary: &Path) -> bool {
    verify_install(binary).is_ok()
}

/// Tell browser discovery about this data directory and how to check a browser in it. Call once
/// at start-up; without it no downloaded browser is ever used.
pub fn register(data_dir: &Path) {
    browser_cdp::launch::register_managed(data_dir, verify_managed);
}

/// The managed browser, if one is installed and passes its manifest check (this hashes the
/// install, so it is not free).
#[must_use]
pub fn installed_browser(data_dir: &Path) -> Option<PathBuf> {
    managed_browser(&root_dir(data_dir), &verify_managed)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Mutex;

    use sha2::{Digest, Sha256};

    use super::*;

    /// A zip shaped like Chrome for Testing: `chrome-linux64/chrome` plus a library.
    fn fake_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let exec = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            w.start_file("chrome-linux64/chrome", exec).unwrap();
            w.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
            w.start_file(
                "chrome-linux64/locales/en-US.pak",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            w.write_all(b"pak").unwrap();
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Network and launch replaced by fakes; counts what happened.
    struct Fake {
        /// What `fetch` writes to disk.
        body: Vec<u8>,
        /// What `start_check` answers.
        start: Result<(), String>,
        /// `fetch` and `start_check` never answer.
        hang: bool,
        fetched: Mutex<u32>,
        started: Mutex<Vec<PathBuf>>,
    }

    impl Fake {
        fn new(body: Vec<u8>) -> Self {
            Self {
                body,
                start: Ok(()),
                hang: false,
                fetched: Mutex::new(0),
                started: Mutex::new(Vec::new()),
            }
        }
    }

    impl Environment for Fake {
        fn fetch(
            &self,
            _asset: &BrowserAsset,
            dest: &Path,
            progress: &(dyn Fn(u64) + Send + Sync),
        ) -> impl Future<Output = Result<(), DownloadError>> + Send {
            *self.fetched.lock().unwrap() += 1;
            let hang = self.hang;
            if !hang {
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::write(dest, &self.body).unwrap();
                progress(self.body.len() as u64);
            }
            async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                Ok(())
            }
        }

        fn start_check(&self, binary: &Path) -> impl Future<Output = Result<(), String>> + Send {
            self.started.lock().unwrap().push(binary.to_owned());
            let (hang, answer) = (self.hang, self.start.clone());
            async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                answer
            }
        }
    }

    /// A pin for the tiny fake archive: its real checksum and size.
    fn pin(zip: &[u8]) -> BrowserAsset {
        BrowserAsset {
            sha256: Box::leak(hex(zip).into_boxed_str()),
            size_bytes: zip.len() as u64,
            ..LINUX_X64
        }
    }

    async fn install_fake(
        data: &Path,
        env: &Fake,
        asset: BrowserAsset,
    ) -> Result<Installed, InstallError> {
        install_asset(data, asset, env, &|_| {}).await
    }

    #[test]
    fn the_pins_match_the_installer_script() {
        let script = include_str!("../../../scripts/install-linux.sh");
        assert!(
            script.contains(&format!("local version=\"{CFT_VERSION}\"")),
            "scripts/install-linux.sh pins another Chrome for Testing version"
        );
        for asset in [LINUX_X64, LINUX_ARM64] {
            assert!(
                script.contains(&format!("=\"{}\"", asset.sha256)),
                "{} checksum differs from scripts/install-linux.sh",
                asset.platform
            );
            assert!(
                asset
                    .url
                    .contains(&format!("/{CFT_VERSION}/{}/", asset.platform)),
                "{}",
                asset.url
            );
            assert!(asset.url.starts_with("https://storage.googleapis.com/"));
            assert_eq!(asset.sha256.len(), 64);
        }
    }

    #[test]
    fn the_install_marker_is_the_one_the_trust_code_writes() {
        assert_eq!(
            MANAGED_MARKER,
            provider_catalog::local_runtime::trusted::MANIFEST_NAME
        );
    }

    #[test]
    fn only_linux_builds_exist_and_an_override_switches_the_offer_off() {
        assert_eq!(asset_for("linux", "x86_64"), Some(LINUX_X64));
        assert_eq!(asset_for("linux", "aarch64"), Some(LINUX_ARM64));
        for (os, arch) in [
            ("windows", "x86_64"),
            ("macos", "aarch64"),
            ("linux", "riscv64"),
        ] {
            assert_eq!(asset_for(os, arch), None, "{os} {arch}");
        }
        assert!(offered("linux", "x86_64", false));
        assert!(!offered("linux", "x86_64", true));
        assert!(!offered("windows", "x86_64", false));
    }

    #[tokio::test]
    async fn installs_a_verified_archive_and_discovery_then_finds_it() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let env = Fake::new(zip.clone());
        let got = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        assert!(!got.already_there);
        assert!(
            got.binary.ends_with("chrome-linux64/chrome"),
            "{:?}",
            got.binary
        );
        assert_eq!(installed_browser(d.path()), Some(got.binary.clone()));
        assert_eq!(*env.started.lock().unwrap(), vec![got.binary.clone()]);
        // The archive is gone once it is unpacked.
        assert!(!root_dir(d.path()).join("downloads").exists());
        // The program is executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&got.binary).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111 & 0o755, "{mode:o}");
        }
    }

    #[tokio::test]
    async fn a_download_with_the_wrong_checksum_installs_nothing() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let mut tampered = zip.clone();
        *tampered.last_mut().unwrap() ^= 0xff;
        // The fake download "arrives" altered; the pin is the checksum of the real bytes.
        let env = Fake::new(tampered);
        let err = install_fake(d.path(), &env, pin(&zip)).await.unwrap_err();
        assert_eq!(err, InstallError::Checksum);
        assert_eq!(installed_browser(d.path()), None);
        let zips = std::fs::read_dir(root_dir(d.path()).join("downloads"))
            .map_or(0, |r| r.flatten().count());
        assert_eq!(zips, 0, "the bad archive is deleted");
        assert!(
            env.started.lock().unwrap().is_empty(),
            "nothing may be started"
        );
        let leftovers: Vec<_> = std::fs::read_dir(root_dir(d.path()))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("i-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn a_second_install_reuses_the_verified_copy() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let env = Fake::new(zip.clone());
        let first = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        let second = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        assert!(second.already_there);
        assert_eq!(second.binary, first.binary);
        assert_eq!(*env.fetched.lock().unwrap(), 1, "no second download");
    }

    #[tokio::test]
    async fn a_changed_install_is_replaced_and_older_folders_are_removed() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let env = Fake::new(zip.clone());
        let first = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        // Someone edits a file of the install: it no longer passes its manifest.
        std::fs::write(
            first.binary.parent().unwrap().join("locales/en-US.pak"),
            b"x",
        )
        .unwrap();
        let second = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        assert!(!second.already_there);
        assert_ne!(second.binary, first.binary);
        assert_eq!(*env.fetched.lock().unwrap(), 2);
        assert!(!first.binary.exists(), "the damaged install is removed");
        assert_eq!(installed_browser(d.path()), Some(second.binary));
    }

    #[tokio::test]
    async fn a_browser_that_does_not_start_is_reported_with_the_reason_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let mut env = Fake::new(zip.clone());
        env.start = Err("the Chrome sandbox cannot start".into());
        let err = install_fake(d.path(), &env, pin(&zip)).await.unwrap_err();
        let InstallError::CannotStart { binary, reason } = &err else {
            panic!("{err:?}");
        };
        assert!(binary.is_file() && reason.contains("sandbox"));
        let text = err.to_string();
        assert!(
            text.contains("downloaded") && text.contains("cannot start"),
            "{text}"
        );
        assert_eq!(installed_browser(d.path()).as_ref(), Some(binary));
    }

    #[tokio::test]
    async fn too_little_disk_space_is_reported_before_downloading() {
        let d = tempfile::tempdir().unwrap();
        let env = Fake::new(Vec::new());
        let huge = BrowserAsset {
            size_bytes: u64::MAX / 8,
            ..LINUX_X64
        };
        let err = install_fake(d.path(), &env, huge).await.unwrap_err();
        assert!(matches!(err, InstallError::NotEnoughDisk { .. }), "{err:?}");
        assert_eq!(*env.fetched.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn an_unsupported_computer_is_refused_before_anything_is_written() {
        let d = tempfile::tempdir().unwrap();
        let env = Fake::new(Vec::new());
        let err = install(d.path(), "windows", "x86_64", &env, &|_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::Unsupported { .. }), "{err:?}");
        assert!(!root_dir(d.path()).exists());
        assert_eq!(*env.fetched.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_second_run_at_the_same_time_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let env = Fake::new(Vec::new());
        let _held = Running::start(d.path()).unwrap();
        let err = install(d.path(), "linux", "x86_64", &env, &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err, InstallError::Busy);
        // Another data directory is not blocked by it.
        let other = tempfile::tempdir().unwrap();
        assert!(Running::start(other.path()).is_ok());
    }

    // ---- Manifest check before a managed browser is used (review finding 1) ----

    #[tokio::test]
    async fn a_planted_install_with_a_marker_and_a_program_is_never_found() {
        let d = tempfile::tempdir().unwrap();
        let root = root_dir(d.path());
        let forged = root.join("i-forged/chrome-linux64");
        std::fs::create_dir_all(&forged).unwrap();
        restrict_dir(&root);
        std::fs::write(forged.join("chrome"), b"#!/bin/sh\necho pwned\n").unwrap();
        // The marker exists (any bytes), the program exists and is executable.
        std::fs::write(root.join("i-forged").join(MANAGED_MARKER), b"junk").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                forged.join("chrome"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert_eq!(installed_browser(d.path()), None);
        // Even a well-formed manifest that does not match the files does not pass.
        std::fs::write(
            root.join("i-forged").join(MANAGED_MARKER),
            format!(
                "praxi-runtime-manifest v1\n{}\tchrome-linux64/chrome\n",
                hex(b"other")
            ),
        )
        .unwrap();
        assert_eq!(installed_browser(d.path()), None);
    }

    #[tokio::test]
    async fn a_real_install_is_found_until_one_byte_of_it_changes() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let env = Fake::new(zip.clone());
        let got = install_fake(d.path(), &env, pin(&zip)).await.unwrap();
        assert_eq!(installed_browser(d.path()), Some(got.binary.clone()));
        // The program itself is swapped...
        std::fs::write(&got.binary, b"#!/bin/sh\necho pwned\n").unwrap();
        assert_eq!(installed_browser(d.path()), None);
    }

    #[tokio::test]
    async fn an_extra_file_dropped_into_a_real_install_is_found_out() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let got = install_fake(d.path(), &Fake::new(zip.clone()), pin(&zip))
            .await
            .unwrap();
        std::fs::write(got.binary.parent().unwrap().join("libevil.so"), b"x").unwrap();
        assert_eq!(installed_browser(d.path()), None);
    }

    // ---- The unpacked folder's guarantees (review finding 4) ----

    use std::io::Cursor;

    /// One entry of a test archive.
    struct Entry<'a> {
        name: &'a str,
        data: &'a [u8],
        mode: Option<u32>,
        /// `Some(target)`: a symlink to it.
        link: Option<&'a str>,
    }

    fn file<'a>(name: &'a str, data: &'a [u8], mode: Option<u32>) -> Entry<'a> {
        Entry {
            name,
            data,
            mode,
            link: None,
        }
    }

    fn zip_of(entries: &[Entry]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for e in entries {
                let mut opts = zip::write::SimpleFileOptions::default();
                if let Some(m) = e.mode {
                    opts = opts.unix_permissions(m);
                }
                if let Some(target) = e.link {
                    w.add_symlink(e.name, target, opts).unwrap();
                } else {
                    w.start_file(e.name, opts).unwrap();
                    w.write_all(e.data).unwrap();
                }
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Install `zip` with a pin made for it; the program is `chrome-linux64/chrome`.
    async fn install_zip(d: &Path, zip: &[u8]) -> Result<Installed, InstallError> {
        install_fake(d, &Fake::new(zip.to_vec()), pin(zip)).await
    }

    fn install_folders(d: &Path) -> Vec<String> {
        std::fs::read_dir(root_dir(d))
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("i-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn an_archive_with_a_path_that_climbs_out_installs_nothing() {
        for evil in [
            "../evil.txt",
            "chrome-linux64/../../evil.txt",
            "chrome-linux64/a/../../../evil.txt",
        ] {
            let d = tempfile::tempdir().unwrap();
            let zip = zip_of(&[
                file("chrome-linux64/chrome", b"x", Some(0o755)),
                file(evil, b"owned", None),
            ]);
            let err = install_zip(d.path(), &zip).await.unwrap_err();
            assert!(matches!(err, InstallError::Unpack(_)), "{evil}: {err:?}");
            assert!(install_folders(d.path()).is_empty(), "{evil}");
            assert!(!root_dir(d.path()).join("evil.txt").exists(), "{evil}");
            assert!(!d.path().join("evil.txt").exists(), "{evil}");
            assert_eq!(installed_browser(d.path()), None);
        }
    }

    #[tokio::test]
    async fn an_archive_with_an_absolute_path_writes_nothing_there() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("outside.txt");
        let name = target.to_string_lossy().into_owned();
        let zip = zip_of(&[
            file("chrome-linux64/chrome", b"x", Some(0o755)),
            file(&name, b"owned", None),
        ]);
        let err = install_zip(d.path(), &zip).await.unwrap_err();
        assert!(matches!(err, InstallError::Unpack(_)), "{err:?}");
        assert!(!target.exists());
        assert_eq!(install_folders(d.path()), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_in_the_archive_is_not_created_and_nothing_is_written_through_it() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let zip = zip_of(&[
            file("chrome-linux64/chrome", b"x", Some(0o755)),
            Entry {
                name: "chrome-linux64/escape",
                data: b"",
                mode: Some(0o777),
                link: Some(outside.path().to_str().unwrap()),
            },
            // Written "through" the link if it had been created.
            file("chrome-linux64/escape/pwned.txt", b"owned", None),
        ]);
        let installed = install_zip(d.path(), &zip).await;
        assert!(!outside.path().join("pwned.txt").exists(), "{installed:?}");
        // Either the install is refused or the folder holds no link.
        if let Ok(done) = installed {
            let escape = done.binary.parent().unwrap().join("escape");
            assert!(!std::fs::symlink_metadata(escape).is_ok_and(|m| m.is_symlink()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_or_hard_link_inside_an_unpacked_folder_is_refused_and_the_folder_removed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"s").unwrap();
        for hard in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().join("browser");
            let top = root.join("i-1/chrome-linux64");
            std::fs::create_dir_all(&top).unwrap();
            std::fs::write(top.join("chrome"), b"x").unwrap();
            if hard {
                std::fs::hard_link(outside.path().join("secret"), top.join("alias")).unwrap();
            } else {
                std::os::unix::fs::symlink(outside.path(), top.join("alias")).unwrap();
            }
            let err = harden_install(&top.join("chrome"), &root, 1 << 20).unwrap_err();
            assert!(matches!(err, InstallError::Unpack(_)), "{err:?}");
            assert!(!root.join("i-1").exists(), "hard={hard}");
            assert!(outside.path().join("secret").exists(), "never followed");
        }
    }

    #[tokio::test]
    async fn an_archive_that_unpacks_to_far_more_than_expected_is_refused_and_removed() {
        let d = tempfile::tempdir().unwrap();
        // Compresses to almost nothing, unpacks to 8 MB: far beyond three times its own size.
        let bomb = vec![0u8; 8 * 1024 * 1024];
        let zip = zip_of(&[
            file("chrome-linux64/chrome", b"x", Some(0o755)),
            file("chrome-linux64/bomb.bin", &bomb, None),
        ]);
        assert!(
            zip.len() * 3 < bomb.len(),
            "the test archive must be a bomb"
        );
        let err = install_zip(d.path(), &zip).await.unwrap_err();
        let InstallError::Unpack(why) = &err else {
            panic!("{err:?}");
        };
        assert!(why.contains("more than"), "{why}");
        assert_eq!(install_folders(d.path()), Vec::<String>::new());
        assert_eq!(installed_browser(d.path()), None);
        // A normal archive of the same shape (small contents) is fine.
        // (Incompressible bytes: a real build unpacks to about twice its archive.)
        let mut x = 0x2545_f491_4f6c_dd1d_u64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect();
        let ok = zip_of(&[file("chrome-linux64/chrome", &noise, Some(0o755))]);
        assert!(install_zip(d.path(), &ok).await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn modes_are_made_sane_and_setuid_setgid_and_sticky_never_survive() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let zip = zip_of(&[
            file("chrome-linux64/chrome", b"x", Some(0o4755)),
            file("chrome-linux64/chrome_sandbox", b"s", Some(0o6777)),
            file("chrome-linux64/sticky", b"t", Some(0o1777)),
            file("chrome-linux64/world.pak", b"w", Some(0o666)),
            file("chrome-linux64/nomode.pak", b"n", None),
            file("chrome-linux64/locales/x.pak", b"l", Some(0o664)),
        ]);
        let done = install_zip(d.path(), &zip).await.unwrap();
        let dir = done.binary.parent().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&done.binary), 0o755);
        assert_eq!(
            mode(&dir.join("chrome_sandbox")),
            0o755,
            "no setuid, no setgid"
        );
        assert_eq!(mode(&dir.join("sticky")), 0o755, "no sticky");
        assert_eq!(mode(&dir.join("world.pak")), 0o644, "no write for others");
        assert_eq!(mode(&dir.join("nomode.pak")), 0o644);
        assert_eq!(mode(&dir.join("locales/x.pak")), 0o644);
        assert_eq!(mode(&dir.join("locales")), 0o755);
        assert_eq!(mode(dir), 0o755);
        // Nothing in the whole install carries a special or a group/other write bit.
        let mut stack = vec![dir.parent().unwrap().to_path_buf()];
        while let Some(p) = stack.pop() {
            let m = mode(&p);
            assert_eq!(m & 0o7022, 0, "{}: {m:o}", p.display());
            if p.is_dir() {
                stack.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
            }
        }
        // The manifest still matches after the modes were changed.
        assert_eq!(installed_browser(d.path()), Some(done.binary));
    }

    // ---- Time limits (review finding 7) ----

    fn quick() -> Limits {
        Limits {
            fetch: std::time::Duration::from_millis(100),
            start: std::time::Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn a_download_that_stalls_is_stopped_and_frees_the_install_slot() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        let mut env = Fake::new(zip.clone());
        env.hang = true;
        let err = install_limited(d.path(), pin(&zip), &env, &|_| {}, quick())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, InstallError::Network(m) if m.contains("longer than")),
            "{err:?}"
        );
        assert!(Running::start(d.path()).is_ok(), "the slot is free again");
        assert_eq!(*env.started.lock().unwrap(), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn a_test_start_that_never_answers_is_reported_and_the_files_stay() {
        let d = tempfile::tempdir().unwrap();
        let zip = fake_zip();
        // Download fine, then hang in the start check only.
        let ok = Fake::new(zip.clone());
        install_limited(d.path(), pin(&zip), &ok, &|_| {}, quick())
            .await
            .unwrap();
        let mut env = Fake::new(zip.clone());
        env.hang = true;
        let err = install_limited(d.path(), pin(&zip), &env, &|_| {}, quick())
            .await
            .unwrap_err();
        let InstallError::CannotStart { reason, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(reason.contains("did not start within"), "{reason}");
        assert!(installed_browser(d.path()).is_some());
    }

    #[test]
    fn errors_say_what_to_do() {
        let disk = InstallError::NotEnoughDisk {
            need_bytes: 1_146_000_000,
            free_bytes: 300_000_000,
        }
        .to_string();
        assert!(
            disk.contains("1146 MB") && disk.contains("300 MB"),
            "{disk}"
        );
        let net = InstallError::from(DownloadError::Network("dns".into())).to_string();
        assert!(
            net.contains("storage.googleapis.com") && net.contains("firewall"),
            "{net}"
        );
        assert_eq!(
            InstallError::from(DownloadError::ChecksumMismatch),
            InstallError::Checksum
        );
        let libs = with_missing_library_note(
            "browser launch failed: the browser is missing system libraries",
        );
        assert!(libs.contains("libnss3"), "{libs}");
        assert_eq!(with_missing_library_note("other"), "other");
    }

    #[test]
    fn the_apt_hint_uses_the_package_names_this_system_has() {
        let release =
            |id: &str, version: &str| format!("NAME=x\nID={id}\nVERSION_ID=\"{version}\"\n");
        for (id, version, suffix) in [
            ("ubuntu", "22.04", ""),
            ("ubuntu", "24.04", "t64"),
            ("ubuntu", "26.04", "t64"),
            ("debian", "12", ""),
            ("debian", "13", "t64"),
        ] {
            let cmd = apt_install_command_for(&release(id, version));
            assert!(
                cmd.starts_with("sudo apt-get install -y libnss3")
                    && cmd.contains(&format!(" libasound2{suffix} "))
                    && cmd.ends_with(&format!(" libatspi2.0-0{suffix}")),
                "{id} {version}: {cmd}"
            );
        }
        // Debian testing and unstable have no VERSION_ID and already use the new names.
        assert!(has_t64_libraries("ID=debian\n"));
        assert!(!has_t64_libraries(""));
    }

    #[test]
    fn the_apt_hint_lists_the_libraries_the_installer_script_installs() {
        let script = include_str!("../../../scripts/install-linux.sh");
        let after = script
            .split("local libs=(")
            .nth(1)
            .expect("install_cft has a libs list");
        let (list, rest) = after.split_once(")\n").expect("the libs list ends");
        let from_script: Vec<&str> = list.split_whitespace().collect();
        assert_eq!(from_script, CHROME_LIBRARIES, "hint and installer differ");
        let loop_line = rest
            .lines()
            .find(|l| l.contains("for pkg in libasound2"))
            .expect("the t64 loop");
        let t64: Vec<&str> = loop_line
            .trim()
            .trim_start_matches("for pkg in ")
            .trim_end_matches("; do")
            .split_whitespace()
            .collect();
        assert_eq!(
            t64, CHROME_LIBRARIES_T64,
            "t64 names differ from the installer"
        );
    }
}
