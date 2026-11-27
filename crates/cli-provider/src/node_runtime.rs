//! A Praxi-owned Node.js runtime for hosts that have none, installed WITHOUT root.
//!
//! The always-on host runs as an unprivileged service user whose only writable
//! place is the data directory. npm-based coding CLIs (Claude Code, Codex,
//! `OpenCode`) need `node` and `npm`, so the host-admin install flow can fetch the
//! official Node.js tarball into `<data>/runtimes/node-vX.Y.Z` and install the
//! CLIs with a user-level npm prefix inside `<data>/runtimes/npm-global`.
//!
//! Version choice: the LTS MAJOR is pinned ([`NODE_MAJOR`]) and the exact patch
//! version is resolved from `latest-v<MAJOR>.x/SHASUMS256.txt` of the same
//! release directory, so the pin never goes stale on a patch release and no
//! checksum table has to be maintained here. The tarball is accepted only when
//! its SHA-256 equals the digest published in that file. (This detects a
//! corrupt or swapped download; it does not defend against nodejs.org itself
//! being compromised: the file's PGP signature is not checked.)
//!
//! Transport: HTTPS only, including every redirect. Download and unpack are
//! size bounded, run under a total time limit, and stage into a temporary
//! directory that is removed on every path.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// The pinned Node.js LTS major.
pub const NODE_MAJOR: u32 = 22;
/// Official distribution root.
pub const DIST_BASE: &str = "https://nodejs.org/dist";
/// Whole install (index, download, unpack) must finish within this.
pub const NODE_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TARBALL_BYTES: u64 = 160 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_UNPACKED_BYTES: u64 = 900 * 1024 * 1024;
const MAX_ENTRIES: usize = 60_000;
const MARKER: &str = "node-current";

static RUNTIME_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Tell this process where the data directory is, so every `PathEnv` built from
/// the ambient environment includes an installed runtime (idempotent).
pub fn register_data_dir(data: &Path) {
    let _ = RUNTIME_ROOT.set(data.to_path_buf());
}

pub(crate) fn registered_data_dir() -> Option<&'static Path> {
    RUNTIME_ROOT.get().map(PathBuf::as_path)
}

/// `linux-x64` / `linux-arm64` for the machine this daemon runs on; `None` when
/// Praxi cannot honestly install Node here (other OS or architecture).
#[must_use]
pub fn supported_target() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        match std::env::consts::ARCH {
            "x86_64" => Some("linux-x64"),
            "aarch64" => Some("linux-arm64"),
            _ => None,
        }
    } else {
        None
    }
}

fn runtimes_dir(data: &Path) -> PathBuf {
    data.join("runtimes")
}

/// npm's user-level global prefix (inside the data directory, never a system path).
#[must_use]
pub fn npm_prefix(data: &Path) -> PathBuf {
    runtimes_dir(data).join("npm-global")
}

/// The installed runtime's `bin` directory, when one is installed and complete.
#[must_use]
pub fn node_bin_dir(data: &Path) -> Option<PathBuf> {
    let name = std::fs::read_to_string(runtimes_dir(data).join(MARKER)).ok()?;
    let name = name.trim();
    if !name.starts_with("node-v")
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
    {
        return None;
    }
    let bin = runtimes_dir(data).join(name).join("bin");
    bin.join("node").is_file().then_some(bin)
}

/// Directories to put at the FRONT of the search path: the runtime's `bin`, then
/// the npm prefix's `bin` (where `npm install -g` puts the CLIs). Empty when no
/// runtime is installed.
#[must_use]
pub fn path_dirs(data: &Path) -> Vec<PathBuf> {
    node_bin_dir(data).map_or_else(Vec::new, |bin| vec![bin, npm_prefix(data).join("bin")])
}

/// What this machine offers for installing npm-based coding CLIs as the current user: the `node`
/// and `npm` the search path finds, and the folder `npm install -g` would write to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toolchain {
    /// The `node` program found on the search path.
    pub node: Option<PathBuf>,
    /// Its version as `node --version` prints it (`v22.23.5`).
    pub node_version: Option<String>,
    /// The `npm` program found on the search path.
    pub npm: Option<PathBuf>,
    /// Where `npm install -g` puts packages: `NPM_CONFIG_PREFIX`, else `prefix=` of `~/.npmrc`,
    /// else npm's default, the folder above the one holding `node` (read, not asked of npm).
    pub prefix: Option<PathBuf>,
    /// The current user can write there (or into the nearest folder that exists above it);
    /// `None` when that cannot be told (no prefix, or not a Unix system).
    pub prefix_writable: Option<bool>,
}

impl Toolchain {
    /// The major version of `node_version` (`22` for `v22.23.5`).
    #[must_use]
    pub fn node_major(&self) -> Option<u32> {
        self.node_version
            .as_deref()?
            .trim_start_matches('v')
            .split('.')
            .next()?
            .parse()
            .ok()
    }
}

/// Look for `node` and `npm` the way installs do (the same search path), run `node --version`
/// (three seconds at most) and work out where `npm install -g` would write. Writes nothing.
#[must_use]
pub fn probe(env: &crate::PathEnv) -> Toolchain {
    let node = crate::detect::find_candidate("node", env);
    let npm = crate::detect::find_candidate("npm", env);
    let node_version = node.as_deref().and_then(|n| node_version(n, env));
    let prefix = env
        .env_vars
        .get("NPM_CONFIG_PREFIX")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env.home_dir
                .as_deref()
                .and_then(|home| npmrc_prefix(&std::fs::read_to_string(home.join(".npmrc")).ok()?))
        })
        .or_else(|| {
            // npm's own default: `<prefix>/bin/node`, so the prefix is two folders up (never `/`).
            let real = std::fs::canonicalize(node.as_deref()?).ok()?;
            let prefix = real.ancestors().nth(2)?;
            prefix.parent().is_some().then(|| prefix.to_path_buf())
        })
        .filter(|p| p.is_absolute());
    let prefix_writable = prefix.as_deref().and_then(writable_or_creatable);
    Toolchain {
        node,
        node_version,
        npm,
        prefix,
        prefix_writable,
    }
}

/// The absolute `prefix=` value of an `.npmrc`'s text (the last one wins, as in npm).
fn npmrc_prefix(text: &str) -> Option<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "prefix").then(|| value.trim().trim_matches(['"', '\'']))
        })
        .next_back()
        .filter(|v| Path::new(v).is_absolute())
        .map(PathBuf::from)
}

fn node_version(node: &Path, env: &crate::PathEnv) -> Option<String> {
    let path = std::env::join_paths(env.path_dirs.iter()).ok()?;
    let mut child = std::process::Command::new(node)
        .arg("--version")
        .env_clear()
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            _ => return None,
        }
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut text).ok()?;
    let version = text.trim();
    (version.starts_with('v') && version.len() < 32 && !version.contains(char::is_whitespace))
        .then(|| version.to_owned())
}

/// Whether the current user can write `dir`, or the nearest folder above it that exists (npm creates
/// the rest). `None` off Unix, where this check is not made.
#[cfg(unix)]
fn writable_or_creatable(dir: &Path) -> Option<bool> {
    let existing = dir.ancestors().find(|p| p.exists())?;
    Some(
        rustix::fs::access(existing, rustix::fs::Access::WRITE_OK).is_ok()
            && existing.metadata().ok()?.is_dir(),
    )
}

#[cfg(not(unix))]
fn writable_or_creatable(_dir: &Path) -> Option<bool> {
    None
}

fn sha256_hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `(version, sha256)` of the tarball for `target` from a `SHASUMS256.txt`.
fn parse_shasums(text: &str, target: &str) -> Option<(String, String)> {
    let suffix = format!("-{target}.tar.gz");
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(sum), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Some(version) = file
            .strip_prefix("node-v")
            .and_then(|f| f.strip_suffix(suffix.as_str()))
        else {
            continue;
        };
        let nums: Vec<&str> = version.split('.').collect();
        let numeric = nums.len() == 3
            && nums
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if numeric
            && nums[0] == NODE_MAJOR.to_string()
            && sum.len() == 64
            && sum.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Some((version.to_owned(), sum.to_ascii_lowercase()));
        }
    }
    None
}

fn client(https_only: bool) -> Result<reqwest::Client, &'static str> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if https_only && attempt.url().scheme() != "https" {
                attempt.error("redirect to a non-https address refused")
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "http_client")
}

async fn fetch_index(http: &reqwest::Client, url: &str) -> Result<String, &'static str> {
    let resp = http.get(url).send().await.map_err(|_| "download_failed")?;
    if !resp.status().is_success() {
        return Err("download_failed");
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.map_err(|_| "download_failed")?);
        if body.len() > MAX_INDEX_BYTES {
            return Err("index_too_large");
        }
    }
    String::from_utf8(body).map_err(|_| "index_invalid")
}

/// Download, verify and unpack the Node.js LTS tarball for `target` below
/// `<data>/runtimes`, make it the current runtime and return its version.
///
/// `base` is the distribution root ([`DIST_BASE`] in production). With
/// `https_only` false (tests against a local server) plain HTTP is allowed.
///
/// # Errors
/// A stable code: `download_failed`, `checksum_missing`, `checksum_mismatch`,
/// `too_large`, `unpack_failed`, `incomplete_runtime`, `timeout`, `io_failed`.
pub async fn install_node(
    base: &str,
    target: &str,
    data: &Path,
    https_only: bool,
) -> Result<String, &'static str> {
    tokio::time::timeout(
        NODE_INSTALL_TIMEOUT,
        install_inner(base, target, data, https_only),
    )
    .await
    .unwrap_or(Err("timeout"))
}

async fn install_inner(
    base: &str,
    target: &str,
    data: &Path,
    https_only: bool,
) -> Result<String, &'static str> {
    if https_only && !base.starts_with("https://") {
        return Err("download_failed");
    }
    let http = client(https_only)?;
    let release = format!("{}/latest-v{NODE_MAJOR}.x", base.trim_end_matches('/'));
    let shasums = fetch_index(&http, &format!("{release}/SHASUMS256.txt")).await?;
    let (version, want) = parse_shasums(&shasums, target).ok_or("checksum_missing")?;
    let file = format!("node-v{version}-{target}.tar.gz");

    let root = runtimes_dir(data);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|_| "io_failed")?;
    let stage = root.join(format!(".node-staging-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|_| "io_failed")?;
    let result = stage_and_publish(&http, &release, &file, &version, &want, &root, &stage).await;
    let _ = tokio::fs::remove_dir_all(&stage).await;
    result
}

async fn stage_and_publish(
    http: &reqwest::Client,
    release: &str,
    file: &str,
    version: &str,
    want: &str,
    root: &Path,
    stage: &Path,
) -> Result<String, &'static str> {
    let resp = http
        .get(format!("{release}/{file}"))
        .send()
        .await
        .map_err(|_| "download_failed")?;
    if !resp.status().is_success() {
        return Err("download_failed");
    }
    if resp.content_length().is_some_and(|n| n > MAX_TARBALL_BYTES) {
        return Err("too_large");
    }
    let tarball = stage.join(file);
    let mut out = tokio::fs::File::create(&tarball)
        .await
        .map_err(|_| "io_failed")?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "download_failed")?;
        total += chunk.len() as u64;
        if total > MAX_TARBALL_BYTES {
            return Err("too_large");
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.map_err(|_| "io_failed")?;
    }
    out.flush().await.map_err(|_| "io_failed")?;
    drop(out);
    if sha256_hex(hasher.finalize()) != want {
        return Err("checksum_mismatch");
    }

    let unpack_to = stage.join("out");
    let (tar_path, dest) = (tarball.clone(), unpack_to.clone());
    tokio::task::spawn_blocking(move || unpack(&tar_path, &dest))
        .await
        .map_err(|_| "unpack_failed")??;
    let top = unpack_to.join(file.trim_end_matches(".tar.gz"));
    if !top.join("bin").join("node").is_file() || !top.join("bin").join("npm").exists() {
        return Err("incomplete_runtime");
    }
    let name = format!("node-v{version}");
    let final_dir = root.join(&name);
    if final_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&final_dir).await;
    }
    tokio::fs::rename(&top, &final_dir)
        .await
        .map_err(|_| "io_failed")?;
    // Publish the marker atomically: a half-written marker never names a runtime.
    let tmp_marker = root.join(format!(".{MARKER}.tmp"));
    tokio::fs::write(&tmp_marker, &name)
        .await
        .map_err(|_| "io_failed")?;
    tokio::fs::rename(&tmp_marker, root.join(MARKER))
        .await
        .map_err(|_| "io_failed")?;
    Ok(version.to_owned())
}

fn unpack(tarball: &Path, dest: &Path) -> Result<(), &'static str> {
    std::fs::create_dir_all(dest).map_err(|_| "io_failed")?;
    let file = std::fs::File::open(tarball).map_err(|_| "io_failed")?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    archive.set_preserve_permissions(true);
    archive.set_unpack_xattrs(false);
    let (mut entries, mut bytes) = (0usize, 0u64);
    for entry in archive.entries().map_err(|_| "unpack_failed")? {
        let mut entry = entry.map_err(|_| "unpack_failed")?;
        entries += 1;
        bytes += entry.header().size().unwrap_or(0);
        if entries > MAX_ENTRIES || bytes > MAX_UNPACKED_BYTES {
            return Err("too_large");
        }
        // `unpack_in` refuses entries that would leave `dest` (`..`, absolute paths).
        entry.unpack_in(dest).map_err(|_| "unpack_failed")?;
    }
    Ok(())
}

/// Run the installed `node --version` once (bounded) and check it names `version`.
///
/// # Errors
/// `node_not_runnable` when it does not start, times out or reports another version.
pub async fn verify_runs(data: &Path, version: &str) -> Result<(), &'static str> {
    let bin = node_bin_dir(data).ok_or("node_not_runnable")?.join("node");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version").kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "node_not_runnable")?
        .map_err(|_| "node_not_runnable")?;
    if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == format!("v{version}")
    {
        Ok(())
    } else {
        Err("node_not_runnable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<root>/bin/{node,npm}` as tiny scripts; `node --version` prints `version`.
    #[cfg(unix)]
    fn fake_toolchain(root: &Path, version: &str, with_npm: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mut programs = vec![("node", format!("#!/bin/sh\necho {version}\n"))];
        if with_npm {
            programs.push(("npm", "#!/bin/sh\necho 10.9.0\n".to_owned()));
        }
        for (name, text) in programs {
            let file = bin.join(name);
            std::fs::write(&file, text).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    #[cfg(unix)]
    #[test]
    fn an_npm_already_on_the_path_beats_the_runtime_in_the_data_folder() {
        let data = tempfile::tempdir().unwrap();
        let runtimes = data.path().join("runtimes");
        let ours = fake_toolchain(&runtimes.join("node-v22.1.0"), "v22.1.0", true);
        std::fs::write(runtimes.join(MARKER), "node-v22.1.0").unwrap();
        // No npm on the search path: the fallback applies (as the app's own installer relies on).
        let none = tempfile::tempdir().unwrap();
        let env = crate::PathEnv::for_test(none.path().to_path_buf())
            .with_node_runtime_fallback(data.path());
        assert_eq!(env.path_dirs[0], ours);
        assert!(env.env_vars.contains_key("NPM_CONFIG_PREFIX"));
        // The installer's Node.js is there: nothing is added, and its prefix is not overridden.
        let theirs = fake_toolchain(&data.path().join("opt-node"), "v22.23.5", true);
        let prefix = data.path().join("home/.npm-global");
        let env = crate::PathEnv::for_test(theirs.clone())
            .with_env_var("NPM_CONFIG_PREFIX", prefix.to_string_lossy())
            .with_node_runtime_fallback(data.path());
        assert_eq!(env.path_dirs, vec![theirs]);
        assert_eq!(
            env.env_vars.get("NPM_CONFIG_PREFIX").map(String::as_str),
            prefix.to_str()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_toolchain_names_node_npm_and_the_users_global_prefix() {
        let root = tempfile::tempdir().unwrap();
        let bin = fake_toolchain(&root.path().join("opt/node"), "v22.23.5", true);
        let prefix = root.path().join("home/.npm-global");
        let env = crate::PathEnv::for_test(bin.clone())
            .with_env_var("NPM_CONFIG_PREFIX", prefix.to_string_lossy());
        let tc = probe(&env);
        assert_eq!(tc.node.as_deref(), Some(bin.join("node").as_path()));
        assert_eq!(tc.npm.as_deref(), Some(bin.join("npm").as_path()));
        assert_eq!(tc.node_version.as_deref(), Some("v22.23.5"));
        assert_eq!(tc.node_major(), Some(22));
        assert_eq!(tc.prefix.as_deref(), Some(prefix.as_path()));
        // Not created yet: the nearest folder that exists (the tempdir) decides, and it is writable.
        assert_eq!(tc.prefix_writable, Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn a_prefix_the_user_cannot_write_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        // The owner's failure: npm's default prefix is the folder above `node`, a system folder.
        let runtime = root.path().join("opt/praxi-nodejs");
        let bin = fake_toolchain(&runtime, "v22.23.5", true);
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o555)).unwrap();
        let tc = probe(&crate::PathEnv::for_test(bin));
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            tc.prefix.as_deref().map(|p| p.canonicalize().unwrap()),
            Some(runtime.canonicalize().unwrap()),
            "npm's default prefix is the folder above bin/node"
        );
        if !rustix::process::geteuid().is_root() {
            assert_eq!(tc.prefix_writable, Some(false));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_prefix_comes_from_npmrc_when_the_environment_has_none() {
        let root = tempfile::tempdir().unwrap();
        let bin = fake_toolchain(&root.path().join("opt/node"), "v20.1.0", true);
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".npmrc"),
            "registry=https://registry.example.test/\nprefix=relative/dir\nprefix = /srv/mine\n",
        )
        .unwrap();
        let env = crate::PathEnv::for_test(bin).with_home(home);
        assert_eq!(probe(&env).prefix, Some(PathBuf::from("/srv/mine")));
        assert_eq!(npmrc_prefix("prefix=relative\n"), None);
        assert_eq!(
            npmrc_prefix("prefix=\"/a b\"\n"),
            Some(PathBuf::from("/a b"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_pieces_are_reported_as_missing() {
        let empty = tempfile::tempdir().unwrap();
        let none = probe(&crate::PathEnv::for_test(empty.path().to_path_buf()));
        assert_eq!(none, Toolchain::default());
        // node without npm: both facts are kept apart.
        let bin = fake_toolchain(&empty.path().join("n"), "v18.0.0", false);
        let tc = probe(&crate::PathEnv::for_test(bin));
        assert!(tc.node.is_some() && tc.npm.is_none());
        assert_eq!(tc.node_major(), Some(18));
        // A node that prints no version leaves the version out instead of guessing.
        let odd = fake_toolchain(&empty.path().join("o"), "not-a-version", true);
        assert_eq!(probe(&crate::PathEnv::for_test(odd)).node_version, None);
    }

    #[test]
    fn shasums_pick_the_pinned_major_and_target_only() {
        let d = "a".repeat(64);
        let text = format!(
            "{d}  node-v22.14.0-linux-arm64.tar.gz\n{d}  node-v22.14.0-linux-x64.tar.gz\n{d}  node-v23.1.0-linux-x64.tar.gz\nxx  node-v22.9.9-linux-x64.tar.gz\n"
        );
        assert_eq!(
            parse_shasums(&text, "linux-x64"),
            Some(("22.14.0".into(), d.clone()))
        );
        assert_eq!(parse_shasums(&text, "linux-ppc64le"), None);
        assert_eq!(parse_shasums("garbage", "linux-x64"), None);
    }

    fn fake_tarball() -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for name in ["bin/node", "bin/npm"] {
                let body = b"#!/bin/sh\necho v22.14.0\n";
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(
                    &mut header,
                    format!("node-v22.14.0-linux-x64/{name}"),
                    &body[..],
                )
                .unwrap();
            }
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    async fn serve(shasum: String, tarball: Vec<u8>) -> String {
        use axum::{Router, routing::get};
        let app = Router::new()
            .route(
                "/latest-v22.x/SHASUMS256.txt",
                get(move || {
                    let text = format!("{shasum}  node-v22.14.0-linux-x64.tar.gz\n");
                    async move { text }
                }),
            )
            .route(
                "/latest-v22.x/node-v22.14.0-linux-x64.tar.gz",
                get(move || {
                    let bytes = tarball.clone();
                    async move { bytes }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    #[tokio::test]
    async fn node_install_refuses_a_bad_checksum_and_a_good_one_extends_the_search_path() {
        let tarball = fake_tarball();
        let good = sha256_hex(Sha256::digest(&tarball));

        // A digest that does not match the download: refused, nothing published, nothing left behind.
        let data = tempfile::tempdir().unwrap();
        let base = serve("0".repeat(64), tarball.clone()).await;
        assert_eq!(
            install_node(&base, "linux-x64", data.path(), false).await,
            Err("checksum_mismatch")
        );
        assert!(node_bin_dir(data.path()).is_none());
        let leftovers: Vec<_> = std::fs::read_dir(data.path().join("runtimes"))
            .unwrap()
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging must be removed: {leftovers:?}"
        );

        // The published digest matches: installed, on the search path, npm resolvable.
        let base = serve(good, tarball).await;
        assert_eq!(
            install_node(&base, "linux-x64", data.path(), false).await,
            Ok("22.14.0".to_owned())
        );
        let bin = node_bin_dir(data.path()).expect("runtime installed");
        let empty = tempfile::tempdir().unwrap();
        let env =
            crate::PathEnv::for_test(empty.path().to_path_buf()).with_node_runtime(data.path());
        assert_eq!(env.path_dirs[0], bin);
        assert_eq!(
            env.env_vars.get("NPM_CONFIG_PREFIX").map(String::as_str),
            Some(npm_prefix(data.path()).to_str().unwrap())
        );
        #[cfg(unix)]
        assert!(
            crate::install::resolve_installer(&env).is_ok(),
            "npm engines are installable now"
        );

        // https is required outside tests: a plain-http base is refused up front.
        assert_eq!(
            install_node(&base, "linux-x64", data.path(), true).await,
            Err("download_failed")
        );
    }
}
