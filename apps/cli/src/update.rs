//! Explicit, anonymous release updates. Never restart a service here.

#[derive(Debug, clap::Args)]
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
        println!("use the installer");
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
    }

    fn tag(version: &str) -> Result<String> {
        let version = version.strip_prefix('v').unwrap_or(version);
        if version.is_empty()
            || version.len() > 100
            || !version.starts_with(|c: char| c.is_ascii_digit())
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
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

    /// Preserve the installer's mandatory provenance policy. The original card's
    /// no-op signature TODO would bypass the protection now present on main.
    async fn verify_signature(archive: &Path, bundle: &Path, version: &str) -> Result<()> {
        let config = tempfile::Builder::new()
            .prefix(".gh-verify-")
            .tempdir_in(archive.parent().ok_or("archive has no parent")?)?;
        let verifier = trusted_verifier()?;
        let status = tokio::process::Command::new(verifier)
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
            // The verifier needs no daemon/provider credentials or account config.
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", config.path())
            .env("GH_CONFIG_DIR", config.path())
            .env("GH_HOST", "github.com")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(std::process::Stdio::null())
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
        // Check every component: a root-owned executable in a writable directory
        // is still replaceable. Never search the caller's PATH, even under sudo.
        let path = Path::new("/usr/bin/gh").canonicalize().map_err(
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
                return Err("untrusted provenance verifier ownership or permissions".into());
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
        if current.is_none() {
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

    #[cfg(unix)]
    fn installed_tag(prefix: &Path) -> Result<String> {
        if let Some(current) = read_version_link(prefix, "current")? {
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
        let prefix = args
            .prefix
            .clone()
            .map_or_else(|| inferred_prefix(&std::env::current_exe()?), Ok)?;
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
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent(concat!("Praxi-Bot/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()?;
        let release = release(&client, API, args.version.as_deref()).await?;
        let name = asset_name(std::env::consts::ARCH)?;
        let archive_asset = select(&release, &name)?;
        let checksum_asset = select(&release, &format!("{name}.sha256"))?;
        let bundle_asset = select(&release, "praxi-linux.sigstore.json")?;
        for asset in [archive_asset, checksum_asset, bundle_asset] {
            let expected = format!(
                "https://github.com/emreocell/Praxi-Bot/releases/download/{}/{}",
                release.tag_name, asset.name
            );
            if asset.browser_download_url != expected {
                return Err("unexpected release asset origin".into());
            }
        }
        let installed = installed_tag(&prefix);
        println!(
            "installed {}; available {} ({name})",
            installed
                .as_deref()
                .unwrap_or("incomplete (repair required)"),
            release.tag_name
        );
        if args.check {
            installed?;
            return Ok(());
        }
        directory(&prefix)?;
        let _lock = lock(&prefix)?;
        directory(&prefix.join("versions"))?;
        // Always download and verify again, including same-tag repair. Directory
        // presence (or an interrupted current switch) is not proof of integrity.
        let staging = tempfile::Builder::new()
            .prefix(".update-")
            .tempdir_in(&prefix)?;
        let archive = staging.path().join(&name);
        download(
            &client,
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
        verify_sha256(&archive, std::str::from_utf8(&checksum)?, &name)?;
        let bundle = staging.path().join("provenance.json");
        download(
            &client,
            &bundle_asset.browser_download_url,
            &bundle,
            16 * 1024 * 1024,
        )
        .await?;
        verify_signature(&archive, &bundle, &release.tag_name).await?;
        let unpacked = staging.path().join("unpacked");
        std::fs::create_dir(&unpacked)?;
        unpack(&archive, &unpacked, std::env::consts::ARCH)?;
        ensure_launchers(&prefix)?;
        let target = publish_version(&prefix, &unpacked, &release.tag_name)?;
        switch(&prefix, &target)?;
        println!(
            "installed {}; restart required (launch {}/current/bin/bot)",
            release.tag_name,
            prefix.display()
        );
        Ok(())
    }

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

    fn lock(prefix: &Path) -> Result<std::fs::File> {
        let path = prefix.join(".update.lock");
        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
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
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for name in ["bot", "bot-node"] {
                let bytes = b"fixture binary";
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(
                        &mut header,
                        format!("praxi-linux-aarch64/bin/{name}"),
                        &bytes[..],
                    )
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
                assert!(verify_signature(&archive, &bundle, "v1.2.3").await.is_err());
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
    }
}
