use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, backup::Backup};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::{
    FileRecord, ImportError, ImportOptions, Manifest, Phase, ProgressCallback, Result, invalid,
    policy::{MAX_POLICY_BYTES, POLICY_FILE},
    progress,
};

/// Recursively included roots (profiles in both legacy and current layouts).
pub const INCLUDED_DIRECTORIES: &[&str] = &["bots", "profiles", "skills", "workflows"];
/// Included files. Schedules actually live in a separate SQLite database.
pub const INCLUDED_FILES: &[&str] = &[
    "bot.db",
    "schedules.db",
    "memory.db",
    "providers.toml",
    "mcp.toml",
    "host.toml",
    POLICY_FILE,
];
/// Excluded at any depth. A trailing `*` means prefix; matching is case insensitive.
pub const EXCLUSIONS: &[&str] = &[
    "api-token",
    "identity.key",
    "devices.json",
    "peer-node",
    "secrets.enc",
    "browsers",
    "browser",
    "browser-profiles",
    "browser_profiles",
    "logs",
    "backup-*",
    "import-staging",
];
const METADATA_CAP: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;

fn excluded(path: &str) -> bool {
    path.split('/').any(|part| {
        EXCLUSIONS.iter().any(|pattern| {
            let part = part.to_ascii_lowercase();
            pattern
                .strip_suffix('*')
                .map_or_else(|| part == *pattern, |prefix| part.starts_with(prefix))
        })
    })
}

pub(crate) fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path.split('/').any(|p| {
            let stem = p.split('.').next().unwrap_or_default().to_ascii_uppercase();
            p.is_empty()
                || p == "."
                || p == ".."
                || p.ends_with(['.', ' '])
                || p.chars().any(char::is_control)
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
        })
    {
        return Err(invalid("unsafe archive path"));
    }
    Ok(())
}

fn allowed(path: &str) -> bool {
    !excluded(path)
        && (INCLUDED_FILES.contains(&path)
            || INCLUDED_DIRECTORIES
                .iter()
                .any(|root| path.starts_with(&format!("{root}/"))))
}

pub(crate) fn collect(dir: &Path) -> Result<Vec<String>> {
    fn walk(dir: &Path, path: &str, files: &mut Vec<String>) -> Result<()> {
        if excluded(path) {
            return Ok(());
        }
        safe_path(path)?;
        let meta = match std::fs::symlink_metadata(dir.join(path)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(invalid("symlink in export"));
        }
        if meta.is_dir() && !INCLUDED_FILES.contains(&path) {
            for entry in std::fs::read_dir(dir.join(path))? {
                let name = entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("non-UTF8 path"))?;
                walk(dir, &format!("{path}/{name}"), files)?;
            }
        } else if meta.is_file() && allowed(path) {
            if files.len() >= MAX_ENTRIES {
                return Err(ImportError::SizeLimit);
            }
            files.push(path.into());
        } else {
            return Err(invalid("unsupported export file type"));
        }
        Ok(())
    }
    let mut files = Vec::new();
    for root in INCLUDED_DIRECTORIES.iter().chain(INCLUDED_FILES) {
        walk(dir, root, &mut files)?;
    }
    Ok(files)
}

pub(crate) fn backup(source: &Path, target: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let mut dest = Connection::open(target)?;
    Backup::new(&conn, &mut dest)?.run_to_completion(128, Duration::from_millis(10), None)?;
    Ok(())
}

pub(crate) async fn write_files(
    data_dir: &Path,
    sink: &mut (impl AsyncWrite + Unpin),
    manifest: &mut Manifest,
    bytes: &mut u64,
    cb: Option<&ProgressCallback>,
) -> Result<()> {
    let policy_json = crate::policy::export(data_dir)?;
    let mut paths = collect(data_dir)?;
    if !paths.iter().any(|path| path == crate::policy::POLICY_FILE) {
        paths.push(crate::policy::POLICY_FILE.into());
    }
    paths.sort();
    // Keep snapshots alive until their payloads have been streamed.
    let snapshots = tempfile::tempdir()?;
    for path in paths {
        if path == crate::policy::POLICY_FILE {
            manifest.files.push(
                write_entry(
                    sink,
                    &path,
                    policy_json.len() as u64,
                    &mut policy_json.as_slice(),
                    bytes,
                    cb,
                )
                .await?,
            );
            continue;
        }
        let mut source = data_dir.join(&path);
        if matches!(path.as_str(), "bot.db" | "schedules.db" | "memory.db") {
            let snapshot = snapshots.path().join(&path);
            backup(&source, &snapshot)?;
            source = snapshot;
            let version = storage::migrate::current_version(&rusqlite::Connection::open(&source)?)?;
            manifest
                .db_schemas
                .get_or_insert_default()
                .insert(path.clone(), version);
            if path == "bot.db" {
                manifest.schema_version = version;
                if manifest.schema_version > storage::migrate::latest_version() {
                    return Err(invalid("database schema is too new"));
                }
            }
        }
        let size = std::fs::metadata(&source)?.len();
        let mut file = tokio::fs::File::open(source).await?;
        manifest
            .files
            .push(write_entry(sink, &path, size, &mut file, bytes, cb).await?);
    }
    if !manifest.files.iter().any(|f| f.path == "bot.db") {
        return Err(invalid("bot.db is required"));
    }
    Ok(())
}

pub(crate) async fn write_entry(
    sink: &mut (impl AsyncWrite + Unpin),
    path: &str,
    size: u64,
    source: &mut (impl AsyncRead + Unpin),
    bytes: &mut u64,
    cb: Option<&ProgressCallback>,
) -> Result<FileRecord> {
    let mut header = tar::Header::new_ustar();
    header.set_path(path)?;
    header.set_size(size);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    sink.write_all(header.as_bytes()).await?;
    *bytes += 512;
    let mut hash = Sha256::new();
    let mut buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    let mut remaining = size;
    while remaining > 0 {
        let len = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        source.read_exact(&mut buffer[..len]).await?;
        hash.update(&buffer[..len]);
        sink.write_all(&buffer[..len]).await?;
        remaining -= len as u64;
        *bytes += len as u64;
        progress(cb, *bytes, Phase::Export);
    }
    let padding = (512 - size % 512) % 512;
    sink.write_all(&[0; 512][..padding as usize]).await?;
    *bytes += padding;
    Ok(FileRecord {
        path: path.into(),
        size,
        sha256: format!("{:x}", hash.finalize()),
    })
}

struct Reader<R> {
    source: R,
    bytes: u64,
    max: u64,
    cb: Option<ProgressCallback>,
}
impl<R: AsyncRead + Unpin> Reader<R> {
    async fn exact(&mut self, dest: &mut [u8]) -> Result<()> {
        let next = self
            .bytes
            .checked_add(dest.len() as u64)
            .ok_or(ImportError::SizeLimit)?;
        if next > self.max {
            return Err(ImportError::SizeLimit);
        }
        self.source.read_exact(dest).await?;
        self.bytes = next;
        progress(self.cb.as_ref(), self.bytes, Phase::Receive);
        Ok(())
    }
}

pub(crate) type Secrets = BTreeMap<String, Zeroizing<String>>;

// Strict USTAR/GNU regular entries only: no extension records, links, devices,
// sparse files, directory metadata, duplicate or case-aliasing names.
pub(crate) async fn receive(
    staging: &Path,
    source: impl AsyncRead + Unpin,
    opts: &ImportOptions,
) -> Result<(Manifest, Secrets, u64)> {
    let mut input = Reader {
        source,
        bytes: 0,
        max: opts.max_bytes,
        cb: opts.progress.clone(),
    };
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    let mut secret_json = Zeroizing::new(Vec::new());
    let manifest;
    loop {
        let mut block = [0; 512];
        input.exact(&mut block).await?;
        if block == [0; 512] {
            return Err(invalid("missing final manifest"));
        }
        let header = tar::Header::from_byte_slice(&block);
        let sum: u32 = block
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u32::from(*b)
                }
            })
            .sum();
        if header.cksum()? != sum {
            return Err(invalid("tar checksum mismatch"));
        }
        if !header.entry_type().is_file() || header.link_name()?.is_some() {
            return Err(invalid("only regular files are accepted"));
        }
        let path = std::str::from_utf8(&header.path_bytes())
            .map_err(|_| invalid("non-UTF8 path"))?
            .to_owned();
        safe_path(&path)?;
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(invalid("duplicate archive path"));
        }
        if seen.len() > MAX_ENTRIES {
            return Err(ImportError::SizeLimit);
        }
        let metadata = path == "manifest.json" || path == "secrets.json";
        if !metadata && !allowed(&path) {
            return Err(invalid("path outside allowlist"));
        }
        let size = header.size()?;
        if size > opts.max_entry_bytes || (metadata && size > METADATA_CAP) {
            return Err(ImportError::SizeLimit);
        }
        if path == POLICY_FILE && size > MAX_POLICY_BYTES {
            return Err(ImportError::SizeLimit);
        }
        // Check the declared size before allocating memory or creating files.
        if input
            .bytes
            .checked_add(size)
            .is_none_or(|n| n > opts.max_bytes)
        {
            return Err(ImportError::SizeLimit);
        }
        let (sha256, memory) = input.payload(staging, &path, size, metadata).await?;
        if path == "manifest.json" {
            manifest = serde_json::from_slice::<Manifest>(&memory)?;
            break;
        }
        if path == "secrets.json" {
            secret_json = memory;
        }
        records.push(FileRecord { path, size, sha256 });
    }
    input.finish().await?;
    verify_manifest(&manifest, &records)?;
    let secrets: Secrets = if secret_json.is_empty() {
        BTreeMap::new()
    } else {
        serde_json::from_slice(&secret_json)?
    };
    if secrets.keys().cloned().collect::<Vec<_>>() != manifest.secret_names {
        return Err(invalid("secret names mismatch"));
    }
    for name in secrets.keys() {
        secret_store::validate_name(name)?;
    }
    Ok((manifest, secrets, input.bytes))
}

fn verify_manifest(manifest: &Manifest, records: &[FileRecord]) -> Result<()> {
    if manifest.format_version != crate::FORMAT_VERSION {
        return Err(invalid("unsupported format version"));
    }
    if manifest.schema_version > storage::migrate::latest_version() {
        return Err(ImportError::IncompatibleSchema {
            db: "bot.db".into(),
            found: manifest.schema_version,
            supported: storage::migrate::latest_version(),
        });
    }
    if manifest.files != records {
        return Err(invalid("manifest size/path/sha256 mismatch"));
    }
    if !records.iter().any(|f| f.path == "bot.db") {
        return Err(invalid("missing bot.db"));
    }
    if !records.iter().any(|f| f.path == POLICY_FILE) {
        return Err(invalid("missing policy.json"));
    }
    Ok(())
}

impl<R: AsyncRead + Unpin> Reader<R> {
    async fn payload(
        &mut self,
        staging: &Path,
        path: &str,
        size: u64,
        metadata: bool,
    ) -> Result<(String, Zeroizing<Vec<u8>>)> {
        let mut memory = Zeroizing::new(Vec::new());
        let mut file = if metadata {
            None
        } else {
            let dest = staging.join(path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Some(
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dest)
                    .await?,
            )
        };
        let mut hash = Sha256::new();
        let mut buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
        let mut remaining = size;
        while remaining > 0 {
            let len = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            self.exact(&mut buffer[..len]).await?;
            hash.update(&buffer[..len]);
            if let Some(file) = &mut file {
                file.write_all(&buffer[..len]).await?;
            } else {
                memory.extend_from_slice(&buffer[..len]);
            }
            remaining -= len as u64;
        }
        if let Some(mut file) = file {
            file.flush().await?;
            file.sync_all().await?;
        }
        let padding = (512 - size % 512) % 512;
        self.exact(&mut buffer[..padding as usize]).await?;
        if buffer[..padding as usize].iter().any(|b| *b != 0) {
            return Err(invalid("nonzero tar padding"));
        }
        Ok((format!("{:x}", hash.finalize()), memory))
    }
}

impl<R: AsyncRead + Unpin> Reader<R> {
    async fn finish(&mut self) -> Result<()> {
        // Require both end markers and consume to EOF, counting all trailing bytes.
        let mut end = [0; 1024];
        self.exact(&mut end).await?;
        if end != [0; 1024] {
            return Err(invalid("manifest must be last"));
        }
        let mut trailing = [0; 4096];
        loop {
            let n = self.source.read(&mut trailing).await?;
            if n == 0 {
                break;
            }
            self.bytes = self
                .bytes
                .checked_add(n as u64)
                .ok_or(ImportError::SizeLimit)?;
            if self.bytes > self.max {
                return Err(ImportError::SizeLimit);
            }
            if trailing[..n].iter().any(|b| *b != 0) {
                return Err(invalid("data after manifest"));
            }
        }
        Ok(())
    }
}
