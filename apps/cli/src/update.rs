//! Verified Linux release installation and managed service updates.

#[derive(Debug, clap::Args)]
#[allow(clippy::struct_excessive_bools)] // independent command-line flags
pub struct UpdateArgs {
    /// Show the available release without downloading or installing it.
    #[arg(long, conflicts_with = "rollback")]
    pub check: bool,
    /// Release tag (a leading v is optional).
    #[arg(long, conflicts_with = "rollback")]
    pub version: Option<String>,
    /// Switch to the previously installed version.
    #[arg(long)]
    pub rollback: bool,
    /// Installation prefix; normally inferred from the installed executable.
    #[arg(long)]
    pub prefix: Option<std::path::PathBuf>,
    /// Port passed by the installer-managed systemd updater units.
    #[arg(long, hide = true, conflicts_with_all = ["check", "version", "rollback", "prefix"])]
    pub managed_port: Option<u16>,
    #[arg(long, hide = true, requires = "managed_port")]
    pub scheduled: bool,
    /// Run by the updater units after every run: mark a run that was killed as failed.
    #[arg(
        long,
        hide = true,
        requires = "managed_port",
        conflicts_with = "scheduled"
    )]
    pub recover: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(clippy::unnecessary_wraps))]
pub fn run(args: &UpdateArgs) -> crate::Result<()> {
    #[cfg(target_os = "linux")]
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(linux::run(args))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        println!(
            "Self-update is only supported on Linux. On this platform, update Praxi using the installer."
        );
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {
    use std::io::Read;
    use std::path::{Component, Path, PathBuf};
    #[cfg(target_os = "linux")]
    use std::time::Duration;

    use serde::Deserialize;
    use sha2::{Digest, Sha256};

    #[cfg(target_os = "linux")]
    use super::UpdateArgs;
    use crate::Result;

    const API: &str = "https://api.github.com/repos/emreocell/Praxi-Bot";
    /// Release assets are accepted from here only: `<ORIGIN>/<tag>/<asset>`.
    const RELEASE_ORIGIN: &str = "https://github.com/emreocell/Praxi-Bot/releases/download";
    const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
    const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;

    #[derive(Debug, Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    #[derive(Debug, Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<Asset>,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }

    fn tag(version: &str) -> Result<String> {
        let version = version.strip_prefix('v').unwrap_or(version);
        if version.is_empty()
            || version.len() > 100
            || !version.starts_with(|c: char| c.is_ascii_digit())
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
            || version.contains("..")
        {
            return Err("invalid release version".into());
        }
        Ok(format!("v{version}"))
    }

    fn asset_name(arch: &str) -> Result<String> {
        match arch {
            "x86_64" | "aarch64" => Ok(format!("praxi-linux-{arch}.tar.gz")),
            _ => Err(format!("unsupported Linux architecture: {arch}").into()),
        }
    }

    fn select<'a>(release: &'a Release, name: &str) -> Result<&'a Asset> {
        let mut matches = release.assets.iter().filter(|a| a.name == name);
        let selected = matches
            .next()
            .ok_or_else(|| format!("release asset missing: {name}"))?;
        if matches.next().is_some() {
            return Err(format!("duplicate release asset: {name}").into());
        }
        Ok(selected)
    }

    async fn release(
        client: &reqwest::Client,
        base: &str,
        version: Option<&str>,
    ) -> Result<Release> {
        let suffix = version
            .map(tag)
            .transpose()?
            .map_or_else(|| "latest".into(), |v| format!("tags/{v}"));
        let response = client
            .get(format!("{base}/releases/{suffix}"))
            .send()
            .await?
            .error_for_status()?;
        let bytes = bounded_body(response, 1024 * 1024).await?;
        let release: Release = serde_json::from_slice(&bytes)?;
        if tag(&release.tag_name)? != release.tag_name {
            return Err("release tag must have a leading v".into());
        }
        if release.draft {
            return Err("refusing a draft release".into());
        }
        if let Some(wanted) = version
            && tag(wanted)? != release.tag_name
        {
            return Err("release tag differs from the requested version".into());
        }
        Ok(release)
    }

    async fn bounded_body(mut response: reqwest::Response, max: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() as u64 + chunk.len() as u64 > max {
                return Err("release response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    // Stream the tarball to disk: do not hold hundreds of MB in RAM on a small host.
    async fn download(client: &reqwest::Client, url: &str, path: &Path, max: u64) -> Result<()> {
        let mut response = client.get(url).send().await?.error_for_status()?;
        let mut file = tokio::fs::File::create(path).await?;
        let mut size = 0;
        while let Some(chunk) = response.chunk().await? {
            size += chunk.len() as u64;
            if size > max {
                return Err("release download too large".into());
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        }
        file.sync_all().await?;
        Ok(())
    }

    fn verify_sha256(path: &Path, checksum: &str, name: &str) -> Result<()> {
        let mut lines = checksum.lines().filter(|l| !l.trim().is_empty());
        let line = lines.next().ok_or("empty checksum")?;
        if lines.next().is_some() {
            return Err("expected a single-asset checksum".into());
        }
        let mut words = line.split_whitespace();
        let expected = words.next().ok_or("empty checksum")?;
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid sha256".into());
        }
        if words.next().map(|file| file.trim_start_matches('*')) != Some(name)
            || words.next().is_some()
        {
            return Err("checksum names a different asset".into());
        }
        let mut file = std::fs::File::open(path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(expected) {
            return Err("sha256 mismatch; installation unchanged".into());
        }
        Ok(())
    }

    /// The oldest GitHub CLI with every identity and offline flag the provenance check relies on
    /// (the installer's `MIN_GH_VERSION`).
    const MIN_GH: (u32, u32) = (2, 80);

    fn gh_version_is_supported(output: &str) -> bool {
        let mut words = output.lines().next().unwrap_or_default().split_whitespace();
        if (words.next(), words.next()) != (Some("gh"), Some("version")) {
            return false;
        }
        let mut parts = words
            .next()
            .unwrap_or_default()
            .split('.')
            .map(str::parse::<u32>);
        matches!(
            (parts.next(), parts.next(), parts.next(), parts.next()),
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(_)), None) if (major, minor) >= MIN_GH
        )
    }

    /// `verifier` as the installer runs it: nothing of the daemon's or the caller's environment
    /// (no tokens, no loader variables, no account config), a fixed `PATH`, and a private `HOME` and
    /// gh config.
    fn verifier_command(verifier: &Path, config: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(verifier);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", config)
            .env("GH_CONFIG_DIR", config)
            .env("GH_HOST", "github.com")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(std::process::Stdio::null());
        command
    }

    /// Preserve the installer's mandatory provenance policy: the archive must be attested by the
    /// release workflow for exactly `refs/tags/<version>`. `verifier` is the pinned, root-owned gh
    /// (see `trusted_verifier`); a checksum alone never authorizes an installation.
    async fn verify_signature(
        verifier: &Path,
        archive: &Path,
        bundle: &Path,
        version: &str,
    ) -> Result<()> {
        let config = tempfile::Builder::new()
            .prefix(".gh-verify-")
            .tempdir_in(archive.parent().ok_or("archive has no parent")?)?;
        let announced = verifier_command(verifier, config.path())
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("provenance verification requires gh attestation: {e}"))?;
        if !announced.status.success()
            || !gh_version_is_supported(&String::from_utf8_lossy(&announced.stdout))
        {
            return Err(format!(
                "provenance verification requires GitHub CLI >= {}.{}.0; installation unchanged",
                MIN_GH.0, MIN_GH.1
            )
            .into());
        }
        let status = verifier_command(verifier, config.path())
            .arg("attestation")
            .arg("verify")
            .arg(archive)
            .arg("--bundle")
            .arg(bundle)
            .args([
                "--repo",
                "emreocell/Praxi-Bot",
                "--hostname",
                "github.com",
                "--signer-workflow",
                "github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml",
                "--source-ref",
                &format!("refs/tags/{version}"),
                "--deny-self-hosted-runners",
                "--cert-oidc-issuer",
                "https://token.actions.githubusercontent.com",
                "--predicate-type",
                "https://slsa.dev/provenance/v1",
                "--digest-alg",
                "sha256",
            ])
            .stdout(std::process::Stdio::null())
            .status()
            .await
            .map_err(|e| format!("provenance verification requires gh attestation: {e}"))?;
        if !status.success() {
            return Err("provenance verification failed; installation unchanged".into());
        }
        Ok(())
    }

    #[cfg(unix)]
    fn trusted_verifier() -> Result<PathBuf> {
        trusted_verifier_at(Path::new("/usr/bin/gh"))
    }

    #[cfg(unix)]
    fn trusted_verifier_at(gh: &Path) -> Result<PathBuf> {
        // Check every component: a root-owned executable in a writable directory
        // is still replaceable. Never search the caller's PATH, even under sudo.
        let path = gh.canonicalize().map_err(
            |_| "provenance verification requires trusted /usr/bin/gh; installation unchanged",
        )?;
        trusted_root_path(&path)?;
        Ok(path)
    }

    #[cfg(not(unix))]
    fn trusted_verifier() -> Result<PathBuf> {
        Err("provenance verification requires Linux".into())
    }

    #[cfg(unix)]
    fn trusted_root_path(path: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        for component in path.ancestors() {
            let meta = std::fs::symlink_metadata(component)?;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.file_type().is_symlink() {
                return Err(format!(
                    "untrusted ownership or permissions at {}",
                    component.display()
                )
                .into());
            }
        }
        Ok(())
    }

    fn unpack(archive: &Path, output: &Path, arch: &str) -> Result<()> {
        let mut tar =
            tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
        let root = format!("praxi-linux-{arch}");
        let mut total = 0_u64;
        let mut count = 0;
        for entry in tar.entries()? {
            let mut entry = entry?;
            count += 1;
            if count > 4096 {
                return Err("too many archive entries".into());
            }
            let path = entry.path()?.into_owned();
            if path
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
                || path.to_string_lossy().contains('\\')
            {
                return Err("unsafe archive path".into());
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| "unexpected archive root")?;
            let kind = entry.header().entry_type();
            if !kind.is_dir() && !kind.is_file() {
                return Err("archive links and special files are forbidden".into());
            }
            total = total
                .checked_add(entry.size())
                .ok_or("archive size overflow")?;
            if total > MAX_UNPACKED {
                return Err("unpacked release too large".into());
            }
            if relative.as_os_str().is_empty() {
                continue;
            }
            let destination = output.join(relative);
            if kind.is_dir() {
                std::fs::create_dir_all(destination)?;
            } else {
                std::fs::create_dir_all(destination.parent().ok_or("invalid archive path")?)?;
                // Refuse duplicates; never follow an archive-created link.
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)?;
                std::io::copy(&mut entry, &mut file)?;
                file.sync_all()?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if relative == Path::new("bin/bot")
                        || relative == Path::new("bin/bot-node")
                    {
                        0o755
                    } else {
                        0o644
                    };
                    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode))?;
                }
            }
        }
        for binary in ["bin/bot", "bin/bot-node"] {
            if !output.join(binary).is_file() {
                return Err(format!("release missing {binary}").into());
            }
        }
        seal_installation(output)?;
        #[cfg(unix)]
        readable_directories(output)?;
        Ok(())
    }

    const INSTALL_SEAL: &str = ".installed-sha256.json";

    fn binary_hashes(directory: &Path) -> Result<std::collections::BTreeMap<String, String>> {
        let mut hashes = std::collections::BTreeMap::new();
        for binary in ["bin/bot", "bin/bot-node"] {
            let path = directory.join(binary);
            let meta = std::fs::symlink_metadata(&path)?;
            if !meta.is_file() || meta.len() == 0 || meta.file_type().is_symlink() {
                return Err("installed binary is missing or incomplete".into());
            }
            let mut file = std::fs::File::open(path)?;
            let mut digest = Sha256::new();
            std::io::copy(&mut file, &mut digest)?;
            hashes.insert(binary.to_owned(), format!("{:x}", digest.finalize()));
        }
        Ok(hashes)
    }

    // Written only after verified extraction (or preservation of the existing
    // installer baseline). This detects interrupted/corrupt installed trees;
    // it is not a substitute for release provenance, which is always reverified.
    fn seal_installation(directory: &Path) -> Result<()> {
        use std::io::Write;
        let bytes = serde_json::to_vec(&binary_hashes(directory)?)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(INSTALL_SEAL))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }

    #[cfg(unix)]
    fn readable_directories(root: &Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    pending.push(entry.path());
                }
            }
            std::fs::File::open(path)?.sync_all()?;
        }
        Ok(())
    }

    fn directory(path: &Path) -> Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(format!("expected an ordinary directory: {}", path.display()).into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(path)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
                }
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn inferred_prefix(exe: &Path) -> Result<PathBuf> {
        let parent = exe
            .parent()
            .and_then(Path::parent)
            .ok_or("cannot infer installation prefix; use --prefix")?;
        if parent
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == "versions"))
        {
            Ok(parent
                .parent()
                .and_then(Path::parent)
                .ok_or("invalid version path")?
                .into())
        } else {
            Ok(parent.into())
        }
    }

    #[cfg(unix)]
    fn read_version_link(prefix: &Path, name: &str) -> Result<Option<PathBuf>> {
        let target = match std::fs::read_link(prefix.join(name)) {
            Ok(target) => target,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut parts = target.components();
        if parts.next() != Some(Component::Normal("versions".as_ref()))
            || parts.clone().count() != 1
        {
            return Err("version symlink escapes the installation".into());
        }
        let version = parts
            .next()
            .ok_or("invalid version link")?
            .as_os_str()
            .to_str()
            .ok_or("invalid version")?;
        if tag(version)? != version {
            return Err("invalid installed version".into());
        }
        let directory = prefix.join(&target);
        if std::fs::symlink_metadata(&directory).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("installed version must not be a symlink".into());
        }
        Ok(Some(target))
    }

    #[cfg(unix)]
    fn replace_link(prefix: &Path, name: &str, target: &Path) -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix(".switch-")
            .tempdir_in(prefix)?;
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(target, &link)?;
        std::fs::rename(link, prefix.join(name))?;
        std::fs::File::open(prefix)?.sync_all()?;
        Ok(())
    }

    #[cfg(unix)]
    fn remove_link(prefix: &Path, name: &str) -> Result<()> {
        match std::fs::remove_file(prefix.join(name)) {
            Ok(()) => std::fs::File::open(prefix)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    #[cfg(unix)]
    fn switch(prefix: &Path, target: &Path) -> Result<()> {
        let old = read_version_link(prefix, "current")?;
        if old.as_deref() == Some(target) {
            return Ok(());
        }
        if let Some(old) = old {
            replace_link(prefix, "previous", &old)?;
        }
        replace_link(prefix, "current", target)
    }

    /// Adopt the existing installer's flat bin layout. Each launcher replacement
    /// is atomic and initially resolves to the same preserved baseline binaries;
    /// only the final `current` switch activates the new release for both.
    #[cfg(unix)]
    fn ensure_launchers(prefix: &Path) -> Result<()> {
        let bin = prefix.join("bin");
        if !bin.try_exists()? {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(&bin)?;
        if metadata.file_type().is_symlink() {
            if std::fs::read_link(&bin)? == Path::new("current/bin") {
                return Ok(());
            }
            return Err("unexpected bin symlink; use a versioned installation".into());
        }
        if !metadata.is_dir() {
            return Err("bin is not a directory".into());
        }
        let current = read_version_link(prefix, "current")?;
        // Flat files are the installer's, newer than anything `current` still names: adopt them again.
        let flat = flat_launchers(prefix);
        for name in ["bot", "bot-node"] {
            let meta = std::fs::symlink_metadata(bin.join(name))?;
            if meta.file_type().is_symlink() {
                if std::fs::read_link(bin.join(name))?
                    != Path::new(&format!("../current/bin/{name}"))
                {
                    return Err("unexpected binary symlink".into());
                }
            } else if !meta.is_file() {
                return Err("launcher must be an ordinary file".into());
            }
        }
        if current.is_none() || flat {
            let installed = std::fs::read_to_string(prefix.join("VERSION"))
                .ok()
                .and_then(|s| s.split_whitespace().next().map(str::to_owned))
                .map(|s| tag(&s))
                .transpose()?
                .unwrap_or(tag(env!("CARGO_PKG_VERSION"))?);
            let baseline = PathBuf::from("versions").join(installed);
            // No launcher has changed until current exists. Rebuild a baseline
            // left by an interrupted adoption from the original flat binaries.
            remove_version(prefix, &baseline)?;
            let staging = tempfile::Builder::new()
                .prefix(".baseline-")
                .tempdir_in(prefix)?;
            std::fs::create_dir(staging.path().join("bin"))?;
            for name in ["bot", "bot-node"] {
                let destination = staging.path().join("bin").join(name);
                std::fs::copy(bin.join(name), &destination)?;
                std::fs::File::open(destination)?.sync_all()?;
            }
            seal_installation(staging.path())?;
            readable_directories(staging.path())?;
            std::fs::rename(staging.path(), prefix.join(&baseline))?;
            std::fs::File::open(prefix.join("versions"))?.sync_all()?;
            replace_link(prefix, "current", &baseline)?;
            // `previous` belonged to the chain this installation replaced.
            remove_link(prefix, "previous")?;
        }
        for name in ["bot", "bot-node"] {
            let staging = tempfile::Builder::new()
                .prefix(".launcher-")
                .tempdir_in(&bin)?;
            let link = staging.path().join(name);
            std::os::unix::fs::symlink(format!("../current/bin/{name}"), &link)?;
            std::fs::rename(link, bin.join(name))?;
        }
        std::fs::File::open(bin)?.sync_all()?;
        Ok(())
    }

    /// The installer's flat layout: `bin/bot` and `bin/bot-node` are ordinary files. A managed
    /// installation has links to `../current/bin/<name>` instead.
    #[cfg(unix)]
    fn flat_launchers(prefix: &Path) -> bool {
        let bin = prefix.join("bin");
        std::fs::symlink_metadata(&bin).is_ok_and(|meta| meta.is_dir())
            && ["bot", "bot-node"].iter().all(|name| {
                std::fs::symlink_metadata(bin.join(name)).is_ok_and(|meta| meta.is_file())
            })
    }

    #[cfg(unix)]
    fn installed_tag(prefix: &Path) -> Result<String> {
        // Flat launchers outrank `current`: `install-linux.sh --update` replaces the files but leaves
        // the links of an earlier managed update, which then name an older release. Trusting them
        // would let a request for that older tag pass as "installed" (a downgrade), and
        // `ensure_launchers` would point the launchers back at it.
        if !flat_launchers(prefix)
            && let Some(current) = read_version_link(prefix, "current")?
        {
            let installed = prefix.join(&current);
            validate_installed(&installed)?;
            if let Some(version) = version_file(&installed)? {
                return Ok(version);
            }
            return Ok(current
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid installed version")?
                .to_owned());
        }
        std::fs::read_to_string(prefix.join("VERSION"))
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_owned))
            .map_or_else(|| tag(env!("CARGO_PKG_VERSION")), |s| tag(&s))
    }

    #[cfg(unix)]
    fn validate_installed(directory: &Path) -> Result<()> {
        let expected = std::fs::read(directory.join(INSTALL_SEAL))
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<std::collections::BTreeMap<String, String>>(&bytes).ok()
            });
        let actual = binary_hashes(directory).ok();
        if expected.is_none() || actual.is_none() || expected != actual {
            return Err(
                "installed version is incomplete or damaged; rerun self-update to repair".into(),
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_lines)] // Keep the verified installation transaction sequential.
    pub(super) async fn run(args: &UpdateArgs) -> Result<()> {
        if let Some(port) = args.managed_port {
            if args.recover {
                return managed::recover().await;
            }
            return managed::run(port, args.scheduled).await;
        }
        let prefix = args
            .prefix
            .clone()
            .map_or_else(|| inferred_prefix(&std::env::current_exe()?), Ok)?;
        let _maintenance = if !args.check
            && prefix
                .canonicalize()
                .as_deref()
                .is_ok_and(|p| p == Path::new("/opt/praxi"))
        {
            Some(maintenance_lock()?)
        } else {
            None
        };
        if !args.check {
            directory(&prefix)?;
            tempfile::Builder::new().prefix(".write-probe-").tempdir_in(&prefix)
                .map_err(|e| format!("installation prefix is not writable: {e}; run sudo {}/bin/bot self-update (with the same options)", prefix.display()))?;
        }
        if args.rollback {
            directory(&prefix)?;
            let _lock = lock(&prefix)?;
            directory(&prefix.join("versions"))?;
            let previous = read_version_link(&prefix, "previous")?
                .ok_or("no previous version to roll back to")?;
            validate_installed(&prefix.join(&previous))?;
            switch(&prefix, &previous)?;
            println!("rolled back to {}; restart required", previous.display());
            return Ok(());
        }
        let client = release_client()?;
        let release = release(&client, API, args.version.as_deref()).await?;
        let [archive, ..] = assets(&release)?;
        let installed = installed_tag(&prefix);
        println!(
            "installed {}; available {} ({})",
            installed
                .as_deref()
                .unwrap_or("incomplete (repair required)"),
            release.tag_name,
            archive.name
        );
        if args.check {
            installed?;
            return Ok(());
        }
        let _lock = lock(&prefix)?;
        let target = prepare(&prefix, &client, &release).await?;
        switch(&prefix, &target)?;
        prune_versions(&prefix);
        println!(
            "installed {}; restart required (launch {}/current/bin/bot)",
            release.tag_name,
            prefix.display()
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn release_client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .https_only(true)
            .user_agent(concat!("Praxi-Bot/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()?)
    }

    #[cfg(target_os = "linux")]
    fn assets(release: &Release) -> Result<[&Asset; 3]> {
        assets_from(release, RELEASE_ORIGIN)
    }

    #[cfg(target_os = "linux")]
    fn assets_from<'a>(release: &'a Release, origin: &str) -> Result<[&'a Asset; 3]> {
        let name = asset_name(std::env::consts::ARCH)?;
        let assets = [
            select(release, &name)?,
            select(release, &format!("{name}.sha256"))?,
            select(release, "praxi-linux.sigstore.json")?,
        ];
        for asset in assets {
            let expected = format!("{origin}/{}/{}", release.tag_name, asset.name);
            if asset.browser_download_url != expected {
                return Err("unexpected release asset origin".into());
            }
        }
        Ok(assets)
    }

    /// Download, verify and publish a release as a new generation under `prefix/versions`. The only
    /// way to an installed version, for the command line and the root updater alike: the archive is
    /// unpacked only after its provenance was verified with the pinned gh, and without that gh
    /// (missing, not root's, too old) nothing is downloaded at all.
    #[cfg(target_os = "linux")]
    async fn prepare(
        prefix: &Path,
        client: &reqwest::Client,
        release: &Release,
    ) -> Result<PathBuf> {
        prepare_from(
            prefix,
            client,
            release,
            RELEASE_ORIGIN,
            &trusted_verifier()?,
        )
        .await
    }

    #[cfg(target_os = "linux")]
    async fn prepare_from(
        prefix: &Path,
        client: &reqwest::Client,
        release: &Release,
        origin: &str,
        verifier: &Path,
    ) -> Result<PathBuf> {
        let [archive_asset, checksum_asset, bundle_asset] = assets_from(release, origin)?;
        let name = &archive_asset.name;
        directory(&prefix.join("versions"))?;
        // Always download and verify again, including same-tag repair. Directory
        // presence (or an interrupted current switch) is not proof of integrity.
        let staging = tempfile::Builder::new()
            .prefix(".update-")
            .tempdir_in(prefix)?;
        let archive = staging.path().join(name);
        download(
            client,
            &archive_asset.browser_download_url,
            &archive,
            MAX_ARCHIVE,
        )
        .await?;
        let checksum = bounded_body(
            client
                .get(&checksum_asset.browser_download_url)
                .send()
                .await?
                .error_for_status()?,
            4096,
        )
        .await?;
        verify_sha256(&archive, std::str::from_utf8(&checksum)?, name)?;
        let bundle = staging.path().join("provenance.json");
        download(
            client,
            &bundle_asset.browser_download_url,
            &bundle,
            16 * 1024 * 1024,
        )
        .await?;
        verify_signature(verifier, &archive, &bundle, &release.tag_name).await?;
        let unpacked = staging.path().join("unpacked");
        std::fs::create_dir(&unpacked)?;
        unpack(&archive, &unpacked, std::env::consts::ARCH)?;
        ensure_launchers(prefix)?;
        publish_version(prefix, &unpacked, &release.tag_name)
    }

    #[cfg(any(target_os = "linux", all(test, windows)))]
    #[path = "update_managed.rs"]
    mod managed;

    #[cfg(unix)]
    fn publish_version(prefix: &Path, unpacked: &Path, version: &str) -> Result<PathBuf> {
        use std::io::Write;

        if tag(version)? != version {
            return Err("invalid published version".into());
        }
        validate_installed(unpacked)?;
        // VERSION is already included in current release archives. Preserve it,
        // and add it for legacy archives so generation names never become the
        // version displayed by self-update --check.
        if let Some(archived) = version_file(unpacked)? {
            if archived != version {
                return Err("archive VERSION differs from the verified release".into());
            }
        } else {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(unpacked.join("VERSION"))?;
            writeln!(file, "{version}")?;
            file.sync_all()?;
        }
        std::fs::File::open(unpacked)?.sync_all()?;

        // Never replace a version directory: current or previous may still
        // reference it, including during same-tag repair. Publish into a fresh
        // generation, then let switch() atomically select it for both launchers.
        let generation = tempfile::Builder::new()
            .prefix("v0-install-")
            .tempdir_in(prefix.join("versions"))?;
        let target = PathBuf::from("versions").join(
            generation
                .path()
                .file_name()
                .ok_or("invalid installation generation")?,
        );
        std::fs::rename(unpacked, generation.path())?;
        std::fs::File::open(prefix.join("versions"))?.sync_all()?;
        let _published = generation.keep();
        Ok(target)
    }

    #[cfg(unix)]
    fn version_file(directory: &Path) -> Result<Option<String>> {
        match std::fs::read_to_string(directory.join("VERSION")) {
            Ok(value) => Ok(Some(tag(value
                .split_whitespace()
                .next()
                .ok_or("empty VERSION")?)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(unix)]
    fn remove_version(prefix: &Path, target: &Path) -> Result<()> {
        let mut parts = target.components();
        if parts.next() != Some(Component::Normal("versions".as_ref()))
            || parts.clone().count() != 1
            || target
                .file_name()
                .and_then(|s| s.to_str())
                .is_none_or(|s| tag(s).ok().as_deref() != Some(s))
        {
            return Err("unsafe version replacement".into());
        }
        let path = prefix.join(target);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                std::fs::remove_dir_all(path)?;
            }
            Ok(_) => return Err("version must be an ordinary directory".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Delete every generation that neither `current` nor `previous` names. Each update publishes a
    /// fresh directory (a same-tag repair too), so without this repeated updates fill the disk.
    /// Best effort: a directory that cannot be removed is reported and kept. Call under the update lock.
    #[cfg(unix)]
    fn prune_versions(prefix: &Path) {
        let mut keep = Vec::new();
        for name in ["current", "previous"] {
            match read_version_link(prefix, name) {
                Ok(link) => keep.extend(link),
                Err(error) => {
                    eprintln!("Praxi update: not pruning old versions: {error}");
                    return;
                }
            }
        }
        // With nothing to keep, nothing is certain: never prune an installation without a baseline.
        let Ok(entries) = std::fs::read_dir(prefix.join("versions")) else {
            return;
        };
        for entry in entries.flatten() {
            let relative = PathBuf::from("versions").join(entry.file_name());
            if keep.is_empty() || keep.contains(&relative) {
                continue;
            }
            // Only directories named like a version or a generation are ours to delete.
            if remove_version(prefix, &relative).is_err() {
                eprintln!("Praxi update: kept {}", relative.display());
            }
        }
    }

    fn lock(prefix: &Path) -> Result<std::fs::File> {
        lock_file(&prefix.join(".update.lock"))
    }

    #[cfg(target_os = "linux")]
    fn maintenance_lock() -> Result<std::fs::File> {
        lock_file(Path::new("/opt/praxi.maintenance.lock"))
    }

    /// Open (creating it if need be) and lock `path`. The file is opened without following a link (a
    /// planted one would otherwise redirect the open, and the creation, of a root-run update), and
    /// what was opened must be a regular file of the user running the update.
    #[cfg(unix)]
    fn lock_file(path: &Path) -> Result<std::fs::File> {
        lock_file_of(path, rustix::process::geteuid().as_raw())
    }

    #[cfg(unix)]
    fn lock_file_of(path: &Path, owner: u32) -> Result<std::fs::File> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("unsafe update lock {}: {e}", path.display()))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != owner {
            return Err(format!(
                "unsafe update lock {}: it must be a regular file of the user running the update",
                path.display()
            )
            .into());
        }
        file.try_lock()
            .map_err(|_| "another update is in progress")?;
        Ok(file)
    }

    #[cfg(not(unix))]
    fn lock_file(path: &Path) -> Result<std::fs::File> {
        if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err("unsafe update lock".into());
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock()
            .map_err(|_| "another update is in progress")?;
        Ok(file)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };

        fn archive() -> Vec<u8> {
            archive_of("praxi-linux-aarch64")
        }

        fn archive_of(root: &str) -> Vec<u8> {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for name in ["bot", "bot-node"] {
                let bytes = b"fixture binary";
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{root}/bin/{name}"), &bytes[..])
                    .unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap()
        }

        #[tokio::test]
        async fn anonymous_release_selection_download_and_checksum_rejection() {
            let server = MockServer::start().await;
            let bytes = archive();
            let name = asset_name("aarch64").unwrap();
            let checksum = format!("{:x}  {name}\n", Sha256::digest(&bytes));
            Mock::given(method("GET")).and(path("/releases/tags/v1.2.3")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v1.2.3", "assets": [
                    {"name": name, "browser_download_url": format!("{}/archive", server.uri())},
                    {"name": format!("{name}.sha256"), "browser_download_url": format!("{}/checksum", server.uri())}
                ]
            }))).mount(&server).await;
            Mock::given(method("GET"))
                .and(path("/archive"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/checksum"))
                .respond_with(ResponseTemplate::new(200).set_body_string(&checksum))
                .mount(&server)
                .await;
            let client = reqwest::Client::new();
            let r = release(&client, &server.uri(), Some("1.2.3"))
                .await
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("archive");
            download(
                &client,
                &select(&r, &name).unwrap().browser_download_url,
                &file,
                MAX_ARCHIVE,
            )
            .await
            .unwrap();
            let sha = client
                .get(
                    &select(&r, &format!("{name}.sha256"))
                        .unwrap()
                        .browser_download_url,
                )
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            verify_sha256(&file, &sha, &name).unwrap();
            let out = dir.path().join("out");
            std::fs::create_dir(&out).unwrap();
            unpack(&file, &out, "aarch64").unwrap();
            assert!(out.join("bin/bot-node").is_file());
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap()
                .write_all(b"tamper")
                .unwrap();
            assert!(
                verify_sha256(&file, &sha, &name)
                    .unwrap_err()
                    .to_string()
                    .contains("mismatch")
            );
            assert!(verify_sha256(&file, &"0".repeat(64), &name).is_err());
            assert!(
                server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .all(|r| !r.headers.contains_key("authorization"))
            );
            assert!(select(&r, "praxi-linux-x86_64.tar.gz").is_err());
        }

        #[test]
        fn versions_and_architectures_are_constrained() {
            assert_eq!(asset_name("x86_64").unwrap(), "praxi-linux-x86_64.tar.gz");
            assert!(asset_name("arm").is_err());
            for v in ["", "../bad", "/tmp", "1/evil", "1..2", "1\\evil"] {
                assert!(tag(v).is_err());
            }
            assert_eq!(tag("1.2.3-rc1").unwrap(), "v1.2.3-rc1");
            assert_eq!(
                inferred_prefix(Path::new("/opt/praxi/versions/v1.2.3/bin/bot")).unwrap(),
                Path::new("/opt/praxi")
            );
            assert_eq!(
                inferred_prefix(Path::new("/opt/praxi/bin/bot")).unwrap(),
                Path::new("/opt/praxi")
            );
        }

        #[test]
        fn rejects_links_and_wrong_archive_roots() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("release.gz");
            std::fs::write(&file, archive()).unwrap();
            assert!(unpack(&file, dir.path(), "x86_64").is_err());
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder
                .append_link(&mut header, "praxi-linux-aarch64/bin/bot", "/etc/passwd")
                .unwrap();
            std::fs::write(&file, builder.into_inner().unwrap().finish().unwrap()).unwrap();
            assert!(
                unpack(&file, dir.path(), "aarch64")
                    .unwrap_err()
                    .to_string()
                    .contains("links")
            );
        }

        #[test]
        fn rejects_archive_traversal_without_writing_outside_staging() {
            let dir = tempfile::tempdir().unwrap();
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes()[..7].copy_from_slice(b"../evil");
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &b"x"[..]).unwrap();
            let file = dir.path().join("malicious.gz");
            std::fs::write(&file, builder.into_inner().unwrap().finish().unwrap()).unwrap();
            let out = dir.path().join("out");
            std::fs::create_dir(&out).unwrap();
            assert!(
                unpack(&file, &out, "aarch64")
                    .unwrap_err()
                    .to_string()
                    .contains("unsafe")
            );
            assert!(!dir.path().join("evil").exists());
        }

        #[test]
        fn update_lock_is_exclusive_and_released_on_drop() {
            let dir = tempfile::tempdir().unwrap();
            let first = lock(dir.path()).unwrap();
            assert!(lock(dir.path()).is_err());
            drop(first);
            assert!(lock(dir.path()).is_ok());
        }

        #[test]
        fn checksum_requires_exactly_the_named_asset() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("asset");
            std::fs::write(&file, b"verified bytes").unwrap();
            let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
            for suffix in ["", "  wrong", "  asset extra"] {
                assert!(verify_sha256(&file, &format!("{hash}{suffix}"), "asset").is_err());
            }
            verify_sha256(&file, &format!("{hash} *asset"), "asset").unwrap();
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn planted_path_verifier_cannot_accept_unverified_bytes() {
            use std::os::unix::fs::PermissionsExt;
            const CHILD: &str = "PRAXI_TEST_PLANTED_VERIFIER";
            if std::env::var_os(CHILD).is_some() {
                let dir = tempfile::tempdir().unwrap();
                let archive = dir.path().join("bad.tar.gz");
                let bundle = dir.path().join("bad.json");
                std::fs::write(&archive, "unverified").unwrap();
                std::fs::write(&bundle, "{}").unwrap();
                let verified = match trusted_verifier() {
                    Ok(verifier) => verify_signature(&verifier, &archive, &bundle, "v1.2.3").await,
                    Err(error) => Err(error),
                };
                assert!(verified.is_err());
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let fake = dir.path().join("gh");
            let marker = dir.path().join("executed");
            std::fs::write(
                &fake,
                format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "update::linux::tests::planted_path_verifier_cannot_accept_unverified_bytes",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("PATH", dir.path())
                .status()
                .unwrap();
            assert!(status.success());
            assert!(
                !marker.exists(),
                "caller PATH must never select the verifier"
            );
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(trusted_root_path(&fake).is_err());
        }

        #[cfg(unix)]
        #[test]
        fn interrupted_adoption_and_incomplete_current_can_be_repaired() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("bin")).unwrap();
            std::fs::create_dir_all(dir.path().join("versions")).unwrap();
            std::fs::write(dir.path().join("VERSION"), "1.0.0").unwrap();
            for name in ["bot", "bot-node"] {
                std::fs::write(dir.path().join("bin").join(name), "baseline").unwrap();
            }
            let target = Path::new("versions/v1.0.0");
            std::fs::create_dir_all(dir.path().join(target).join("bin")).unwrap();
            std::fs::write(dir.path().join(target).join("bin/bot"), "partial").unwrap();
            ensure_launchers(dir.path()).unwrap();
            assert_eq!(installed_tag(dir.path()).unwrap(), "v1.0.0");
            std::fs::write(dir.path().join(target).join("bin/bot-node"), "truncated").unwrap();
            assert!(
                installed_tag(dir.path()).is_err(),
                "nonempty partial binary must also be detected"
            );
            std::fs::remove_file(dir.path().join(target).join("bin/bot-node")).unwrap();
            assert!(installed_tag(dir.path()).is_err());
            // Same operations used after freshly verified unpacking in run().
            ensure_launchers(dir.path()).unwrap();
            let file = dir.path().join("archive.gz");
            std::fs::write(&file, archive()).unwrap();
            let unpacked = dir.path().join("unpacked");
            std::fs::create_dir(&unpacked).unwrap();
            unpack(&file, &unpacked, "aarch64").unwrap();
            let repaired = publish_version(dir.path(), &unpacked, "v1.0.0").unwrap();
            switch(dir.path(), &repaired).unwrap();
            assert_eq!(installed_tag(dir.path()).unwrap(), "v1.0.0");
            // A fully unpacked release left before current switch is also repairable.
            std::fs::remove_file(dir.path().join("current")).unwrap();
            switch(dir.path(), &repaired).unwrap();
            assert_eq!(
                std::fs::read(dir.path().join("bin/bot-node")).unwrap(),
                b"fixture binary"
            );
        }

        #[cfg(unix)]
        #[test]
        fn failed_same_version_publication_preserves_the_running_installation() {
            let dir = tempfile::tempdir().unwrap();
            let active = Path::new("versions/v1.0.0");
            let installed = dir.path().join(active);
            std::fs::create_dir_all(installed.join("bin")).unwrap();
            for name in ["bot", "bot-node"] {
                std::fs::write(installed.join("bin").join(name), "running binary").unwrap();
            }
            seal_installation(&installed).unwrap();
            switch(dir.path(), active).unwrap();
            // Model a filesystem failure after verification but before the
            // staged directory can be published. The current version must
            // remain launchable even when the final rename fails.
            assert!(
                publish_version(dir.path(), &dir.path().join("lost-staging"), "v1.0.0").is_err()
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "running binary"
            );
            assert_eq!(installed_tag(dir.path()).unwrap(), "v1.0.0");
        }

        #[cfg(unix)]
        #[test]
        fn same_tag_publication_keeps_active_and_previous_until_atomic_switch() {
            let dir = tempfile::tempdir().unwrap();
            let active = Path::new("versions/v1.0.0");
            let previous = Path::new("versions/v0.9.0");
            for (target, contents) in [(previous, "previous"), (active, "running")] {
                let installed = dir.path().join(target);
                std::fs::create_dir_all(installed.join("bin")).unwrap();
                for name in ["bot", "bot-node"] {
                    std::fs::write(installed.join("bin").join(name), contents).unwrap();
                }
                seal_installation(&installed).unwrap();
                switch(dir.path(), target).unwrap();
            }
            let file = dir.path().join("archive.gz");
            std::fs::write(&file, archive()).unwrap();
            let unpacked = dir.path().join("unpacked");
            std::fs::create_dir(&unpacked).unwrap();
            unpack(&file, &unpacked, "aarch64").unwrap();
            std::fs::write(unpacked.join("VERSION"), "v1.0.0 signed-commit\n").unwrap();
            let published = publish_version(dir.path(), &unpacked, "v1.0.0").unwrap();

            assert_ne!(published, active);
            assert_eq!(
                read_version_link(dir.path(), "current").unwrap().as_deref(),
                Some(active)
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "running"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("previous/bin/bot")).unwrap(),
                "previous"
            );
            validate_installed(&dir.path().join(&published)).unwrap();

            switch(dir.path(), &published).unwrap();
            assert_eq!(installed_tag(dir.path()).unwrap(), "v1.0.0");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "fixture binary"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("previous/bin/bot")).unwrap(),
                "running"
            );
            switch(dir.path(), active).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "running"
            );
        }

        #[cfg(unix)]
        #[test]
        fn publication_rejects_a_mismatched_archive_version() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("versions")).unwrap();
            let file = dir.path().join("archive.gz");
            std::fs::write(&file, archive()).unwrap();
            let unpacked = dir.path().join("unpacked");
            std::fs::create_dir(&unpacked).unwrap();
            unpack(&file, &unpacked, "aarch64").unwrap();
            std::fs::write(unpacked.join("VERSION"), "v9.9.9").unwrap();
            let error = publish_version(dir.path(), &unpacked, "v1.0.0").unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("differs from the verified release")
            );
            assert!(unpacked.join("bin/bot").exists());
            assert_eq!(
                std::fs::read_dir(dir.path().join("versions"))
                    .unwrap()
                    .count(),
                0
            );
        }

        #[cfg(unix)]
        #[test]
        fn flat_installer_launchers_follow_atomic_current_and_rollback() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("bin")).unwrap();
            std::fs::create_dir(dir.path().join("versions")).unwrap();
            for name in ["bot", "bot-node"] {
                std::fs::write(dir.path().join("bin").join(name), "baseline").unwrap();
            }
            ensure_launchers(dir.path()).unwrap();
            // Re-running adoption is harmless, including after a partial update.
            ensure_launchers(dir.path()).unwrap();
            let next = Path::new("versions/v99.0.0");
            std::fs::create_dir_all(dir.path().join(next).join("bin")).unwrap();
            for name in ["bot", "bot-node"] {
                std::fs::write(dir.path().join(next).join("bin").join(name), "new").unwrap();
            }
            switch(dir.path(), next).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("bin/bot")).unwrap(),
                "new"
            );
            let old = read_version_link(dir.path(), "previous").unwrap().unwrap();
            switch(dir.path(), &old).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("bin/bot-node")).unwrap(),
                "baseline"
            );
        }

        #[cfg(unix)]
        #[test]
        fn symlink_switch_and_rollback_preserve_versions_and_reject_escape() {
            let dir = tempfile::tempdir().unwrap();
            for version in ["v1.0.0", "v2.0.0"] {
                let bin = dir.path().join("versions").join(version).join("bin");
                std::fs::create_dir_all(&bin).unwrap();
                for name in ["bot", "bot-node"] {
                    std::fs::write(bin.join(name), version).unwrap();
                }
            }
            switch(dir.path(), Path::new("versions/v1.0.0")).unwrap();
            switch(dir.path(), Path::new("versions/v2.0.0")).unwrap();
            let previous = read_version_link(dir.path(), "previous").unwrap().unwrap();
            switch(dir.path(), &previous).unwrap();
            assert_eq!(
                std::fs::read_link(dir.path().join("current")).unwrap(),
                Path::new("versions/v1.0.0")
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "v1.0.0"
            );
            replace_link(dir.path(), "previous", Path::new("../escape")).unwrap();
            assert!(read_version_link(dir.path(), "previous").is_err());
        }

        /// An installation as `bot self-update` leaves it after two managed updates.
        #[cfg(unix)]
        fn managed_prefix(dir: &Path) {
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::create_dir_all(dir.join("versions")).unwrap();
            std::fs::write(dir.join("VERSION"), "v1.0.0\n").unwrap();
            for name in ["bot", "bot-node"] {
                std::fs::write(dir.join("bin").join(name), "v1 flat").unwrap();
            }
            ensure_launchers(dir).unwrap();
            for tag in ["v2.0.0", "v3.0.0"] {
                let generation = dir.join("versions").join(tag).join("bin");
                std::fs::create_dir_all(&generation).unwrap();
                for name in ["bot", "bot-node"] {
                    std::fs::write(generation.join(name), tag).unwrap();
                }
                seal_installation(&dir.join("versions").join(tag)).unwrap();
                switch(dir, &Path::new("versions").join(tag)).unwrap();
            }
        }

        /// What `install-linux.sh --update` does: replace the launcher links by flat files and
        /// write VERSION, without touching `current`, `previous` or `versions/`.
        #[cfg(unix)]
        fn installer_update(dir: &Path, version: &str) {
            for name in ["bot", "bot-node"] {
                std::fs::remove_file(dir.join("bin").join(name)).unwrap();
                std::fs::write(dir.join("bin").join(name), format!("{version} flat")).unwrap();
            }
            std::fs::write(dir.join("VERSION"), format!("{version}\n")).unwrap();
        }

        #[cfg(unix)]
        #[test]
        fn a_flat_installer_update_after_managed_updates_is_not_a_downgrade_target() {
            let dir = tempfile::tempdir().unwrap();
            managed_prefix(dir.path());
            assert_eq!(installed_tag(dir.path()).unwrap(), "v3.0.0");
            // The administrator installs v5.0.0 with the verified installer. The stale links still
            // name v3.0.0 (current) and v2.0.0 (previous).
            installer_update(dir.path(), "v5.0.0");
            assert_eq!(
                read_version_link(dir.path(), "current").unwrap().as_deref(),
                Some(Path::new("versions/v3.0.0"))
            );
            // Before anything is adopted, the flat files already decide what is installed...
            assert_eq!(installed_tag(dir.path()).unwrap(), "v5.0.0");
            // ...so a peer asking for the stale v3.0.0 or the intermediate v4.0.0 is a downgrade.
            for stale in ["v3.0.0", "v4.0.0", "v2.0.0"] {
                assert!(!managed::newer_release(
                    stale,
                    &installed_tag(dir.path()).unwrap()
                ));
            }
            // The next managed update adopts the installer's files instead of re-pointing the
            // launchers at the stale release, and drops the history of the replaced chain.
            ensure_launchers(dir.path()).unwrap();
            assert_eq!(installed_tag(dir.path()).unwrap(), "v5.0.0");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("bin/bot")).unwrap(),
                "v5.0.0 flat"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot-node")).unwrap(),
                "v5.0.0 flat"
            );
            assert!(read_version_link(dir.path(), "previous").unwrap().is_none());
            validate_installed(&dir.path().join("current")).unwrap();
            // Whatever the stale chain left behind is now pruned.
            prune_versions(dir.path());
            let mut left: Vec<_> = std::fs::read_dir(dir.path().join("versions"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            left.sort();
            assert_eq!(left, ["v5.0.0"]);
        }

        #[cfg(unix)]
        #[test]
        fn the_same_release_installed_flat_again_replaces_its_own_stale_baseline() {
            let dir = tempfile::tempdir().unwrap();
            managed_prefix(dir.path());
            // The installer re-installs the very tag that `current` names (v3.0.0), other bytes.
            installer_update(dir.path(), "v3.0.0");
            ensure_launchers(dir.path()).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("current/bin/bot")).unwrap(),
                "v3.0.0 flat"
            );
            assert_eq!(installed_tag(dir.path()).unwrap(), "v3.0.0");
        }

        #[cfg(unix)]
        #[test]
        fn pruning_keeps_current_and_previous_and_only_deletes_version_directories() {
            let dir = tempfile::tempdir().unwrap();
            managed_prefix(dir.path());
            let versions = dir.path().join("versions");
            for generation in ["v0-install-abc123", "v0-install-def456", "v1.0.0-old"] {
                std::fs::create_dir_all(versions.join(generation).join("bin")).unwrap();
            }
            // Not ours: a plain file, a directory that is not named like a version, and a link.
            std::fs::write(versions.join("notes.txt"), "keep").unwrap();
            std::fs::create_dir(versions.join("lost+found")).unwrap();
            std::os::unix::fs::symlink("/", versions.join("v9.9.9")).unwrap();
            prune_versions(dir.path());
            let mut left: Vec<_> = std::fs::read_dir(&versions)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            left.sort();
            assert_eq!(
                left,
                ["lost+found", "notes.txt", "v2.0.0", "v3.0.0", "v9.9.9"]
            );
            assert!(Path::new("/").exists());
            // With no baseline nothing is certain, so nothing is deleted.
            remove_link(dir.path(), "current").unwrap();
            remove_link(dir.path(), "previous").unwrap();
            prune_versions(dir.path());
            assert!(versions.join("v2.0.0").exists() && versions.join("v3.0.0").exists());
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn repeated_same_tag_repairs_do_not_fill_the_disk() {
            let dir = tempfile::tempdir().unwrap();
            managed_prefix(dir.path());
            for round in 0..6 {
                let file = dir.path().join("archive.gz");
                std::fs::write(&file, archive()).unwrap();
                let unpacked = dir.path().join(format!("unpacked-{round}"));
                std::fs::create_dir(&unpacked).unwrap();
                unpack(&file, &unpacked, "aarch64").unwrap();
                let target = publish_version(dir.path(), &unpacked, "v3.0.0").unwrap();
                switch(dir.path(), &target).unwrap();
                prune_versions(dir.path());
                // Only what a rollback needs: the new generation and the one it replaced.
                let count = std::fs::read_dir(dir.path().join("versions"))
                    .unwrap()
                    .count();
                assert!(count <= 2, "round {round}: {count} generations");
            }
            validate_installed(&dir.path().join("current")).unwrap();
            validate_installed(&dir.path().join("previous")).unwrap();
        }

        /// A verifier that logs how it was started and answers like gh: `--version` with `version`,
        /// and a verification with success unless `fail` exists.
        #[cfg(unix)]
        struct FakeGh {
            dir: tempfile::TempDir,
        }

        #[cfg(unix)]
        impl FakeGh {
            fn new(version: &str) -> Self {
                use std::os::unix::fs::PermissionsExt;
                let dir = tempfile::tempdir().unwrap();
                let script = format!(
                    "#!/bin/sh\n{{\n  printf 'ARGS=%s\\n' \"$*\"\n  env\n}} >> '{log}'\n\
                     if [ \"$1\" = --version ]; then echo 'gh version {version} (2025-01-01)'; exit 0; fi\n\
                     [ ! -e '{fail}' ]\n",
                    log = dir.path().join("log").display(),
                    fail = dir.path().join("fail").display(),
                );
                std::fs::write(dir.path().join("gh"), script).unwrap();
                std::fs::set_permissions(
                    dir.path().join("gh"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                Self { dir }
            }
            fn path(&self) -> PathBuf {
                self.dir.path().join("gh")
            }
            fn failing(self) -> Self {
                std::fs::write(self.dir.path().join("fail"), "").unwrap();
                self
            }
            fn log(&self) -> String {
                std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
            }
        }

        /// A release served by a local server, for `prepare_from`. The archive is as the host's
        /// architecture names it; a checksum that matches it.
        #[cfg(target_os = "linux")]
        async fn served_release(server: &MockServer, archive: Vec<u8>, tag: &str) -> Release {
            let name = asset_name(std::env::consts::ARCH).unwrap();
            let checksum = format!("{:x}  {name}\n", Sha256::digest(&archive));
            let serve = |file: String, body: Vec<u8>| {
                Mock::given(method("GET"))
                    .and(path(format!("/{tag}/{file}")))
                    .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            };
            serve(name.clone(), archive).mount(server).await;
            serve(format!("{name}.sha256"), checksum.into_bytes())
                .mount(server)
                .await;
            serve("praxi-linux.sigstore.json".into(), b"{}".to_vec())
                .mount(server)
                .await;
            let asset = |file: String| Asset {
                browser_download_url: format!("{}/{tag}/{file}", server.uri()),
                name: file,
            };
            Release {
                tag_name: tag.into(),
                assets: vec![
                    asset(name.clone()),
                    asset(format!("{name}.sha256")),
                    asset("praxi-linux.sigstore.json".into()),
                ],
                draft: false,
                prerelease: false,
            }
        }

        #[cfg(target_os = "linux")]
        fn unsafe_archive() -> Vec<u8> {
            // A header that names `../evil`: unpacking this is an error of its own.
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes()[..7].copy_from_slice(b"../evil");
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &b"x"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap()
        }

        #[cfg(target_os = "linux")]
        fn generations(prefix: &Path) -> usize {
            std::fs::read_dir(prefix.join("versions")).map_or(0, Iterator::count)
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn a_release_is_verified_with_the_installers_policy_before_it_is_published() {
            let server = MockServer::start().await;
            let archive = archive_of(&format!("praxi-linux-{}", std::env::consts::ARCH));
            let release = served_release(&server, archive, "v1.2.3").await;
            let gh = FakeGh::new("2.82.0");
            let prefix = tempfile::tempdir().unwrap();
            let published = prepare_from(
                prefix.path(),
                &reqwest::Client::new(),
                &release,
                &server.uri(),
                &gh.path(),
            )
            .await
            .unwrap();
            validate_installed(&prefix.path().join(&published)).unwrap();

            let log = gh.log();
            // Exactly the identity the installer demands, for this tag only.
            let verify = log
                .lines()
                .find(|line| line.starts_with("ARGS=attestation verify"))
                .expect("gh attestation verify was never run");
            for expected in [
                " --bundle ",
                " --repo emreocell/Praxi-Bot",
                " --hostname github.com",
                " --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml",
                " --source-ref refs/tags/v1.2.3 ",
                " --deny-self-hosted-runners",
                " --cert-oidc-issuer https://token.actions.githubusercontent.com",
                " --predicate-type https://slsa.dev/provenance/v1",
                " --digest-alg sha256",
            ] {
                assert!(
                    verify.contains(expected),
                    "missing {expected:?} in {verify}"
                );
            }
            assert!(!log.contains("refs/heads"), "{log}");
            // Nothing of the daemon's or the caller's environment reaches it.
            let allowed = [
                "PATH",
                "HOME",
                "GH_CONFIG_DIR",
                "GH_HOST",
                "GH_PROMPT_DISABLED",
                "GH_NO_UPDATE_NOTIFIER",
                "PWD",
                "SHLVL",
                "OLDPWD",
                "_",
            ];
            for line in log
                .lines()
                .filter(|l| !l.starts_with("ARGS=") && l.contains('='))
            {
                let key = line.split('=').next().unwrap();
                assert!(allowed.contains(&key), "{key} leaked into the verifier");
            }
            assert!(log.lines().any(|l| l == "PATH=/usr/bin:/bin"));
            let home = std::env::var("HOME").unwrap_or_default();
            assert!(!home.is_empty() && !log.contains(&format!("HOME={home}\n")));
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn a_release_that_fails_provenance_is_never_unpacked_or_published() {
            let server = MockServer::start().await;
            // An archive whose unpacking would fail with an error of its own: the error that comes
            // back must be the provenance one, because that check has to run first.
            let release = served_release(&server, unsafe_archive(), "v1.2.3").await;
            let gh = FakeGh::new("2.82.0").failing();
            let prefix = tempfile::tempdir().unwrap();
            let error = prepare_from(
                prefix.path(),
                &reqwest::Client::new(),
                &release,
                &server.uri(),
                &gh.path(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("provenance verification failed"), "{error}");
            assert_eq!(generations(prefix.path()), 0);
            assert!(
                std::fs::read_dir(prefix.path())
                    .unwrap()
                    .flatten()
                    .all(|entry| !entry.file_name().to_string_lossy().starts_with(".update-")),
                "the staging directory was left behind"
            );
            // The same for a good archive: nothing is published without the attestation.
            let good = MockServer::start().await;
            let archive = archive_of(&format!("praxi-linux-{}", std::env::consts::ARCH));
            let release = served_release(&good, archive, "v1.2.3").await;
            assert!(
                prepare_from(
                    prefix.path(),
                    &reqwest::Client::new(),
                    &release,
                    &good.uri(),
                    &gh.path(),
                )
                .await
                .is_err()
            );
            assert_eq!(generations(prefix.path()), 0);
            // And a verifier that passes lets the unsafe archive reach the unpacking, which refuses it.
            let passing = FakeGh::new("2.82.0");
            let release = served_release(&server, unsafe_archive(), "v1.2.4").await;
            let error = prepare_from(
                prefix.path(),
                &reqwest::Client::new(),
                &release,
                &server.uri(),
                &passing.path(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("unsafe archive path"), "{error}");
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn a_missing_or_old_gh_fails_closed_and_publishes_nothing() {
            let server = MockServer::start().await;
            let archive = archive_of(&format!("praxi-linux-{}", std::env::consts::ARCH));
            let release = served_release(&server, archive, "v1.2.3").await;
            let prefix = tempfile::tempdir().unwrap();
            let client = reqwest::Client::new();
            // No gh at all.
            let missing = prefix.path().join("no-such-gh");
            assert!(
                prepare_from(prefix.path(), &client, &release, &server.uri(), &missing)
                    .await
                    .is_err()
            );
            // One older than the installer's floor (it lacks flags the policy needs).
            let old = FakeGh::new("2.79.9");
            let error = prepare_from(prefix.path(), &client, &release, &server.uri(), &old.path())
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("GitHub CLI >= 2.80.0"), "{error}");
            assert!(
                !old.log().contains("ARGS=attestation"),
                "an old gh must not be asked to verify"
            );
            assert_eq!(generations(prefix.path()), 0);
        }

        #[cfg(unix)]
        #[test]
        fn the_pinned_gh_must_exist_and_belong_to_root_in_root_directories() {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                trusted_verifier_at(&dir.path().join("gh")).is_err(),
                "missing"
            );
            let planted = FakeGh::new("2.82.0");
            // Executable, but in a directory anybody can plant files in (and not root's, or `/tmp`).
            assert!(trusted_verifier_at(&planted.path()).is_err());
        }

        #[test]
        fn only_a_gh_version_with_the_identity_flags_is_supported() {
            for good in [
                "gh version 2.80.0 (2025-08-12)\nhttps://github.com/cli/cli/releases/tag/v2.80.0",
                "gh version 2.82.1",
                "gh version 3.0.0",
            ] {
                assert!(gh_version_is_supported(good), "{good}");
            }
            for bad in [
                "gh version 2.79.9",
                "gh version 1.99.0",
                "gh version 2.80",
                "gh version 2.80.0-rc1",
                "gh version 2.80.0.1",
                "git version 2.80.0",
                "gh 2.80.0",
                "",
            ] {
                assert!(!gh_version_is_supported(bad), "{bad:?}");
            }
        }

        #[cfg(unix)]
        #[test]
        fn a_lock_is_never_opened_through_a_link_and_must_be_the_users_own_regular_file() {
            use std::os::unix::fs::symlink;
            let dir = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let victim = outside.path().join("victim");
            std::fs::write(&victim, "keep").unwrap();
            // A link to an existing file and a dangling one (an open that creates would create it).
            symlink(&victim, dir.path().join("to-file.lock")).unwrap();
            symlink(
                outside.path().join("fresh"),
                dir.path().join("dangling.lock"),
            )
            .unwrap();
            for name in ["to-file.lock", "dangling.lock"] {
                let error = lock_file(&dir.path().join(name)).unwrap_err().to_string();
                assert!(error.contains("unsafe update lock"), "{name}: {error}");
            }
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
            assert!(
                !outside.path().join("fresh").exists(),
                "a lock was created through a link"
            );
            // A directory and a pipe (which must not block the open).
            std::fs::create_dir(dir.path().join("dir.lock")).unwrap();
            assert!(lock_file(&dir.path().join("dir.lock")).is_err());
            rustix::fs::mknodat(
                rustix::fs::CWD,
                dir.path().join("pipe.lock"),
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap();
            assert!(lock_file(&dir.path().join("pipe.lock")).is_err());
            // Somebody else's file (the user running the update is not its owner).
            let plain = dir.path().join("plain.lock");
            drop(lock_file(&plain).unwrap());
            let me = rustix::process::geteuid().as_raw();
            let error = lock_file_of(&plain, me + 1).unwrap_err().to_string();
            assert!(
                error.contains("regular file of the user running the update"),
                "{error}"
            );
            // And an ordinary one is created, locked and released as before.
            let first = lock_file(&plain).unwrap();
            assert!(lock_file(&plain).is_err());
            drop(first);
            assert!(lock_file(&plain).is_ok());
        }
    }
}
