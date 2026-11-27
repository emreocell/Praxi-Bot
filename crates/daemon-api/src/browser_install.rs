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
//! result like any system browser, after the system ones.
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
    verified_binary,
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

/// A browser that cannot load a shared library is the commonest failure on a bare server image:
/// say which package family to install (the installer script does this with root).
fn with_missing_library_note(message: &str) -> String {
    if message.contains("missing system libraries")
        || message.to_ascii_lowercase().contains("shared libraries")
    {
        format!(
            "{message}. Install the libraries Chrome needs (on Ubuntu or Debian: sudo apt-get install -y libnss3 libatk-bridge2.0-0 libcups2 libgbm1 libxkbcommon0 libasound2 libxdamage1 libxfixes3 libxrandr2 libpango-1.0-0 libcairo2), or run `sudo ./install-linux.sh --update --with-desktop`"
        )
    } else {
        message.to_owned()
    }
}

/// Data directories with an install under way: one at a time per directory.
static RUNNING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

struct Running(PathBuf);

impl Running {
    fn start(data_dir: &Path) -> Result<Self, InstallError> {
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

/// Disk the unpacked browser takes, in archive sizes (about 2.1 in practice).
const UNPACKED_FACTOR: u64 = 3;
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
        None => (fetch_and_unpack(&root, asset, env, progress).await?, false),
    };
    env.start_check(&binary)
        .await
        .map_err(|reason| InstallError::CannotStart {
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
    env.fetch(&asset, &archive, progress).await?;

    let (a, r, sha) = (archive.clone(), root.to_owned(), asset.sha256);
    let binary = tokio::task::spawn_blocking(move || {
        // `install_verified` hashes the very file it then unpacks, so a download that was
        // swapped after `fetch` (or a fake that skipped the check) is still refused.
        install_verified(ArchiveKind::Zip, &a, sha, &r, "chrome")
    })
    .await
    .map_err(|e| InstallError::Io(e.to_string()))?;
    let binary = match binary {
        Ok(b) => b,
        Err(e) => {
            // A bad archive is thrown away, never kept for another try.
            if e == TrustError::ArchiveMismatch {
                let _ = std::fs::remove_file(&archive);
            }
            return Err(e.into());
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

/// The managed browser, if one is installed (discovery only; nothing is hashed or started).
#[must_use]
pub fn installed_browser(data_dir: &Path) -> Option<PathBuf> {
    managed_browser(&root_dir(data_dir))
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
        fetched: Mutex<u32>,
        started: Mutex<Vec<PathBuf>>,
    }

    impl Fake {
        fn new(body: Vec<u8>) -> Self {
            Self {
                body,
                start: Ok(()),
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
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &self.body).unwrap();
            progress(self.body.len() as u64);
            std::future::ready(Ok(()))
        }

        fn start_check(&self, binary: &Path) -> impl Future<Output = Result<(), String>> + Send {
            self.started.lock().unwrap().push(binary.to_owned());
            std::future::ready(self.start.clone())
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
}
