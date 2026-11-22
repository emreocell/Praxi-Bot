//! `bot backup create|verify|restore` (passphrase-encrypted single-file
//! backups) and the legacy `bot restore` for older plaintext archives.
//!
//! The encrypted container lives in [`crate::backup_crypto`]; the payload is the
//! `state-bundle` stream. Secrets (API keys, tokens) are only included with an
//! explicit `--include-secrets`, and are only written back by a restore that is
//! also given `--include-secrets`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand};
use secret_store::{MemoryStore, SecretStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use state_bundle::{
    ExportOptions, FORMAT_VERSION, FileRecord, ImportError, ImportOptions, Manifest,
    SecretSelection,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::backup_crypto::{Kdf, SealReader, SealWriter};

/// Name of the advisory lock file a running daemon holds in its data directory.
const DAEMON_LOCK: &str = ".daemon.lock";
const MIN_PASSPHRASE_CHARS: usize = 8;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Subcommand)]
pub enum BackupAction {
    /// Write an encrypted backup of the data directory to one file.
    ///
    /// The passphrase is asked for with hidden input, or read from
    /// `--passphrase-file` or the environment variable named by
    /// `--passphrase-env`; it is never accepted as a command-line value.
    /// Existing files are never overwritten. API keys are excluded unless
    /// `--include-secrets` is given.
    Create(CreateCmd),
    /// Decrypt a backup and check every file against its manifest, without touching the data directory.
    Verify(VerifyCmd),
    /// Restore an encrypted backup into the data directory (stop the daemon first).
    Restore(RestoreEncryptedCmd),
}

/// Where the passphrase comes from (default: a hidden prompt).
#[derive(Args, Clone, Default)]
pub struct PassphraseArgs {
    /// Read the passphrase from the first line of this file.
    #[arg(long, value_name = "FILE", conflicts_with = "passphrase_env")]
    pub passphrase_file: Option<PathBuf>,
    /// Read the passphrase from the environment variable with this name.
    #[arg(long, value_name = "VAR")]
    pub passphrase_env: Option<String>,
}

#[derive(Args)]
pub struct CreateCmd {
    /// Destination backup file path (must not exist yet), e.g. `praxi.bak`.
    pub file: PathBuf,
    #[command(flatten)]
    pub passphrase: PassphraseArgs,
    /// Also include API keys and tokens from the secret store (stored encrypted in the backup).
    #[arg(long)]
    pub include_secrets: bool,
}

#[derive(Args)]
pub struct VerifyCmd {
    /// Encrypted backup file.
    pub file: PathBuf,
    #[command(flatten)]
    pub passphrase: PassphraseArgs,
}

#[derive(Args)]
pub struct RestoreEncryptedCmd {
    /// Encrypted backup file.
    pub file: PathBuf,
    #[command(flatten)]
    pub passphrase: PassphraseArgs,
    /// Replace existing bots and chats (the previous state is kept in a `backup-*` folder inside the data directory).
    #[arg(long)]
    pub replace: bool,
    /// Allow restoring a backup whose permission policy is more permissive than the current one.
    #[arg(long)]
    pub allow_policy_relaxation: bool,
    /// Write the API keys stored in the backup back into the secret store (default: skip them).
    #[arg(long)]
    pub include_secrets: bool,
}

/// Legacy plaintext restore (`bot restore`), for archives made by older versions.
#[derive(Args)]
pub struct RestoreCmd {
    /// Plaintext backup file created by an older `bot backup`.
    pub file: PathBuf,
    /// Replace existing bots and chats (previous state is kept in a `backup-*` folder inside the data directory).
    #[arg(long)]
    pub replace: bool,
    /// Allow restoring a backup whose permission policy is more permissive than the current one.
    #[arg(long)]
    pub allow_policy_relaxation: bool,
}

impl BackupAction {
    /// Whether the action needs the real secret store.
    pub fn needs_secret_store(&self) -> bool {
        match self {
            Self::Create(c) => c.include_secrets,
            Self::Restore(r) => r.include_secrets,
            Self::Verify(_) => false,
        }
    }
}

/// Resolve the passphrase. Never from argv; never logged.
fn read_passphrase(args: &PassphraseArgs, creating: bool) -> crate::Result<Zeroizing<String>> {
    let raw = if let Some(path) = &args.passphrase_file {
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| {
            format!(
                "cannot read passphrase file {}: {}",
                path.display(),
                e.kind()
            )
        })?);
        let line = text.split_inclusive('\n').next().unwrap_or("");
        let line = line.strip_suffix('\n').unwrap_or(line);
        Zeroizing::new(line.strip_suffix('\r').unwrap_or(line).to_owned())
    } else if let Some(var) = &args.passphrase_env {
        Zeroizing::new(
            std::env::var(var).map_err(|_| {
                format!("environment variable {var} is not set (or not valid text)")
            })?,
        )
    } else {
        let mut prompt = dialoguer::Password::new().with_prompt(if creating {
            "New backup passphrase"
        } else {
            "Backup passphrase"
        });
        if creating {
            prompt = prompt.with_confirmation("Repeat passphrase", "Passphrases do not match");
        }
        Zeroizing::new(prompt.interact().map_err(|_| {
            "cannot ask for a passphrase here; use `--passphrase-file <FILE>` or `--passphrase-env <VAR>`"
        })?)
    };
    if raw.is_empty() {
        return Err("the passphrase is empty".into());
    }
    if creating && raw.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(
            format!("the passphrase must have at least {MIN_PASSPHRASE_CHARS} characters").into(),
        );
    }
    Ok(raw)
}

/// Run a `bot backup ...` action. `store` is only read/written with `--include-secrets`.
pub fn run(
    action: &BackupAction,
    data_dir: &Path,
    port: u16,
    store: &dyn SecretStore,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    match action {
        BackupAction::Create(c) => create(data_dir, store, c, json_out, out),
        BackupAction::Verify(c) => verify(c, json_out, out),
        BackupAction::Restore(c) => restore_encrypted(data_dir, port, store, c, json_out, out),
    }
}

fn open_backup(file: &Path) -> crate::Result<std::fs::File> {
    std::fs::File::open(file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "backup file {} not found; check the path and try again",
                file.display()
            )
        } else {
            format!("cannot open backup file {}: {}", file.display(), e.kind())
        }
        .into()
    })
}

fn create(
    data_dir: &Path,
    store: &dyn SecretStore,
    cmd: &CreateCmd,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    create_with_kdf(data_dir, store, cmd, Kdf::default(), json_out, out)
}

fn create_with_kdf(
    data_dir: &Path,
    store: &dyn SecretStore,
    cmd: &CreateCmd,
    kdf: Kdf,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    if !data_dir.join("bot.db").is_file() {
        return Err(format!(
            "nothing to back up: no Praxi data found in {}; run `bot setup` first or pass `--data-dir <PATH>` if your data lives elsewhere",
            data_dir.display()
        )
        .into());
    }
    // Check the destination first so a taken name fails before any prompt.
    let placeholder = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&cmd.file)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "backup file {} already exists; choose a new path (existing files are never overwritten)",
                    cmd.file.display()
                )
            } else {
                format!(
                    "cannot create backup file {}: {}",
                    cmd.file.display(),
                    e.kind()
                )
            }
        })?;
    drop(placeholder);
    let partial = partial_path(&cmd.file);
    let cleanup = |e: Box<dyn std::error::Error>| {
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(&cmd.file);
        e
    };
    let passphrase = read_passphrase(&cmd.passphrase, true).map_err(cleanup)?;
    let result = (|| -> crate::Result<Manifest> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)?;
        let secrets = if cmd.include_secrets {
            SecretSelection::All
        } else {
            SecretSelection::None
        };
        let manifest = crate::current_thread_runtime()?.block_on(seal_export(
            data_dir,
            store,
            secrets,
            file,
            &passphrase,
            kdf,
        ))?;
        std::fs::rename(&partial, &cmd.file)?;
        Ok(manifest)
    })();
    let manifest = result.map_err(cleanup)?;
    let bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
    let secrets = !manifest.secret_names.is_empty();
    if json_out {
        writeln!(
            out,
            "{}",
            json!({
                "file": cmd.file.to_string_lossy(),
                "files": manifest.files.len(),
                "bytes": bytes,
                "encrypted": true,
                "secrets_included": secrets,
            })
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "Encrypted backup written to {} ({} files, {} bytes before encryption).",
        cmd.file.display(),
        manifest.files.len(),
        bytes
    )?;
    if secrets {
        writeln!(
            out,
            "It includes {} API key(s)/token(s). Anyone with the file and the passphrase can read them.",
            manifest.secret_names.len()
        )?;
    } else {
        writeln!(
            out,
            "It does not contain API keys or tokens (add `--include-secrets` to include them)."
        )?;
    }
    writeln!(
        out,
        "Keep the passphrase safe: without it the backup cannot be opened."
    )?;
    Ok(())
}

/// Stream `state_bundle::export` through the encrypting writer.
async fn seal_export(
    data_dir: &Path,
    store: &dyn SecretStore,
    secrets: SecretSelection,
    sink: std::fs::File,
    passphrase: &str,
    kdf: Kdf,
) -> crate::Result<Manifest> {
    let (tx, mut rx) = tokio::io::duplex(128 * 1024);
    let export = state_bundle::export(
        data_dir,
        store,
        ExportOptions {
            secrets,
            ..ExportOptions::default()
        },
        tx,
    );
    let seal = async {
        let mut writer = SealWriter::new(sink, passphrase, kdf)?;
        let mut buf = Zeroizing::new(vec![0u8; 64 * 1024]);
        loop {
            let n = rx.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
        }
        let file = writer.finish()?;
        file.sync_all()?;
        Ok::<(), std::io::Error>(())
    };
    let (exported, sealed) = tokio::join!(export, seal);
    let manifest = exported?;
    sealed?;
    Ok(manifest)
}

fn partial_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// What a decrypted and checked backup holds.
struct Inspection {
    manifest: Manifest,
    bytes: u64,
}

struct HashWriter(Sha256, u64);

impl Write for HashWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.update(data);
        self.1 += data.len() as u64;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Decrypt the whole container and check the tar stream against its manifest.
/// Nothing is written to disk.
fn inspect(file: &Path, passphrase: &str) -> crate::Result<Inspection> {
    let reader = SealReader::open(open_backup(file)?, passphrase)?;
    let mut archive = tar::Archive::new(reader);
    let mut seen: Vec<FileRecord> = Vec::new();
    let mut manifest: Option<Manifest> = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if manifest.is_some() {
            return Err("the backup has entries after its manifest".into());
        }
        if !entry.header().entry_type().is_file() {
            return Err("the backup contains an entry that is not a regular file".into());
        }
        let path = entry
            .path()?
            .to_str()
            .ok_or("the backup contains a non-UTF-8 path")?
            .to_owned();
        let unsafe_path = path.is_empty()
            || path.starts_with('/')
            || path.contains(['\\', ':'])
            || path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..");
        if unsafe_path {
            return Err("the backup contains an unsafe path".into());
        }
        if seen.len() >= 100_000 {
            return Err("the backup has too many entries".into());
        }
        if path == "manifest.json" {
            let mut json = Vec::new();
            (&mut entry)
                .take(MAX_MANIFEST_BYTES + 1)
                .read_to_end(&mut json)?;
            if json.len() as u64 > MAX_MANIFEST_BYTES {
                return Err("the backup manifest is too large".into());
            }
            manifest = Some(serde_json::from_slice(&json).map_err(|_| "invalid backup manifest")?);
        } else {
            let mut hasher = HashWriter(Sha256::new(), 0);
            std::io::copy(&mut entry, &mut hasher)?;
            seen.push(FileRecord {
                path,
                size: hasher.1,
                sha256: hex(&hasher.0.finalize()),
            });
        }
    }
    // Read to the end so the final chunk is authenticated even when the tar
    // reader stopped at its end blocks.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
    let manifest = manifest.ok_or("the backup has no manifest")?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(format!(
            "unsupported backup format version {}; this Praxi reads version {FORMAT_VERSION}",
            manifest.format_version
        )
        .into());
    }
    if manifest.files != seen {
        return Err("the backup contents do not match its manifest".into());
    }
    let bytes = manifest.files.iter().map(|f| f.size).sum();
    Ok(Inspection { manifest, bytes })
}

fn verify(cmd: &VerifyCmd, json_out: bool, out: &mut dyn Write) -> crate::Result<()> {
    let passphrase = read_passphrase(&cmd.passphrase, false)?;
    let found = inspect(&cmd.file, &passphrase)?;
    let m = &found.manifest;
    if json_out {
        writeln!(
            out,
            "{}",
            json!({
                "file": cmd.file.to_string_lossy(),
                "files": m.files.len(),
                "bytes": found.bytes,
                "app_version": m.app_version,
                "schema_version": m.schema_version,
                "created_at": m.created_at,
                "secrets_included": !m.secret_names.is_empty(),
            })
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "Backup {} is intact: {} files, {} bytes (created with Praxi {}, schema {}).",
        cmd.file.display(),
        m.files.len(),
        found.bytes,
        m.app_version,
        m.schema_version
    )?;
    if m.secret_names.is_empty() {
        writeln!(out, "It does not contain API keys or tokens.")?;
    } else {
        writeln!(
            out,
            "It contains {} API key(s)/token(s); they are restored only with `--include-secrets`.",
            m.secret_names.len()
        )?;
    }
    Ok(())
}

fn restore_encrypted(
    data_dir: &Path,
    port: u16,
    store: &dyn SecretStore,
    cmd: &RestoreEncryptedCmd,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let _lock = ensure_daemon_stopped(data_dir, port)?;
    let passphrase = read_passphrase(&cmd.passphrase, false)?;
    // First pass: decrypt and check the entire file before the data dir is
    // touched, so a wrong passphrase or damaged file can never overwrite state.
    let found = inspect(&cmd.file, &passphrase)?;
    let m = &found.manifest;
    if !json_out {
        writeln!(
            out,
            "Backup: {} files, {} bytes, created with Praxi {}.",
            m.files.len(),
            found.bytes,
            m.app_version
        )?;
        if data_dir.join("bot.db").is_file() {
            writeln!(
                out,
                "Replacing the bots, chats, memory, skills, workflows and settings in {} (the current state is kept in a `backup-*` folder).",
                data_dir.display()
            )?;
        }
    }
    let opts = ImportOptions {
        replace: cmd.replace,
        allow_policy_relaxation: cmd.allow_policy_relaxation,
        ..ImportOptions::default()
    };
    // Without `--include-secrets` the secrets of the backup go to a throwaway
    // in-memory store and are dropped.
    let discard = MemoryStore::new();
    let target: &dyn SecretStore = if cmd.include_secrets { store } else { &discard };
    let source = open_backup(&cmd.file)?;
    let outcome = crate::current_thread_runtime()?
        .block_on(unseal_import(data_dir, target, source, &passphrase, opts))
        .map_err(explain_import_error)?;
    let secrets_written = if cmd.include_secrets {
        outcome.secrets_written.len()
    } else {
        0
    };
    let secrets_skipped = if cmd.include_secrets {
        0
    } else {
        m.secret_names.len()
    };
    if json_out {
        writeln!(
            out,
            "{}",
            json!({
                "file": cmd.file.to_string_lossy(),
                "files": outcome.manifest.files.len(),
                "app_version": outcome.manifest.app_version,
                "previous_state": outcome.backup_dir.as_deref().map(Path::to_string_lossy),
                "secrets_restored": secrets_written,
                "secrets_skipped": secrets_skipped,
            })
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "Restored {} files from {} (created with Praxi {}).",
        outcome.manifest.files.len(),
        cmd.file.display(),
        outcome.manifest.app_version
    )?;
    if let Some(previous) = outcome.backup_dir {
        writeln!(
            out,
            "The previous state was kept in {}.",
            previous.display()
        )?;
    }
    if secrets_written > 0 {
        writeln!(out, "Restored {secrets_written} API key(s)/token(s).")?;
    } else if secrets_skipped > 0 {
        writeln!(
            out,
            "The backup holds {secrets_skipped} API key(s)/token(s) that were skipped; run again with `--include-secrets` to restore them."
        )?;
    } else {
        writeln!(
            out,
            "No API keys were in the backup: enter them again (`bot secrets set`, or the app's settings)."
        )?;
    }
    writeln!(
        out,
        "Paired devices and the API token are not part of a backup: pair devices again if you use them."
    )?;
    Ok(())
}

/// Stream the decrypted payload into `state_bundle::import`.
async fn unseal_import(
    data_dir: &Path,
    store: &dyn SecretStore,
    source: std::fs::File,
    passphrase: &str,
    opts: ImportOptions,
) -> crate::Result<state_bundle::ImportOutcome> {
    let (mut tx, rx) = tokio::io::duplex(128 * 1024);
    let import = state_bundle::import(data_dir, store, rx, opts);
    let unseal = async {
        let mut reader = SealReader::open(source, passphrase)?;
        let mut buf = Zeroizing::new(vec![0u8; 64 * 1024]);
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            tx.write_all(&buf[..n]).await?;
        }
        tx.shutdown().await?;
        Ok::<(), std::io::Error>(())
    };
    let (imported, unsealed) = tokio::join!(import, unseal);
    match imported {
        Ok(outcome) => {
            unsealed?;
            Ok(outcome)
        }
        // A decryption failure explains the import error that follows from it.
        Err(e) => match unsealed {
            Err(u) if u.kind() == std::io::ErrorKind::InvalidData => Err(u.into()),
            _ => Err(e.into()),
        },
    }
}

fn explain_import_error(e: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    match e.downcast::<ImportError>() {
        Ok(err) => match *err {
            ImportError::TargetNotEmpty => "this installation already has bots or chats; re-run with `--replace` to swap them for the backup (the current state is kept in a `backup-*` folder)".into(),
            ImportError::PolicyRelaxation => "the backup's permission policy is more permissive than the current one; re-run with `--allow-policy-relaxation` to accept it".into(),
            other => other.to_string().into(),
        },
        Err(other) => other,
    }
}

/// Hold the daemon lock file in `data_dir` for the life of the returned file.
/// `bot serve` calls this at start; a second holder is told about it.
pub fn hold_daemon_lock(data_dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(data_dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data_dir.join(DAEMON_LOCK))
        .ok()?;
    if file.try_lock().is_ok() {
        Some(file)
    } else {
        eprintln!(
            "warning: another Praxi process appears to be using {}",
            data_dir.display()
        );
        None
    }
}

/// Refuse while a daemon uses the data directory: it holds the lock file or
/// answers on `port`. The returned guard keeps the lock during the restore.
fn ensure_daemon_stopped(data_dir: &Path, port: u16) -> crate::Result<std::fs::File> {
    if daemon_listening(port) {
        return Err(format!(
            "cannot restore while a daemon is running on port {port}; stop `bot serve` (or close the desktop app) and retry"
        )
        .into());
    }
    std::fs::create_dir_all(data_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data_dir.join(DAEMON_LOCK))?;
    file.try_lock().map_err(|_| {
        format!(
            "cannot restore while a daemon is using {}; stop `bot serve` (or close the desktop app) and retry",
            data_dir.display()
        )
    })?;
    Ok(file)
}

/// Restore a legacy plaintext archive into `data_dir`. The daemon must be stopped.
pub fn restore(
    data_dir: &Path,
    port: u16,
    store: &dyn SecretStore,
    cmd: &RestoreCmd,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let _lock = ensure_daemon_stopped(data_dir, port)?;
    let source = open_backup(&cmd.file)?;
    let opts = ImportOptions {
        replace: cmd.replace,
        allow_policy_relaxation: cmd.allow_policy_relaxation,
        ..ImportOptions::default()
    };
    let outcome = crate::current_thread_runtime()?
        .block_on(state_bundle::import(
            data_dir,
            store,
            tokio::fs::File::from_std(source),
            opts,
        ))
        .map_err(|e| explain_import_error(e.into()))?;
    if json_out {
        writeln!(
            out,
            "{}",
            json!({
                "file": cmd.file.to_string_lossy(),
                "files": outcome.manifest.files.len(),
                "app_version": outcome.manifest.app_version,
                "previous_state": outcome.backup_dir.as_deref().map(Path::to_string_lossy),
            })
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "Restored {} files from {} (created with Praxi {}).",
        outcome.manifest.files.len(),
        cmd.file.display(),
        outcome.manifest.app_version
    )?;
    if let Some(previous) = outcome.backup_dir {
        writeln!(
            out,
            "The previous state was kept in {}.",
            previous.display()
        )?;
    }
    writeln!(
        out,
        "API keys are not part of a plaintext backup: enter them again (`bot secrets set`, or the app's settings)."
    )?;
    Ok(())
}

fn daemon_listening(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Kdf = Kdf {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };
    const PASS: &str = "correct horse battery";

    fn data_with_bot(dir: &Path) {
        let storage = storage::Storage::open(dir).unwrap();
        let user = protocol::UserId::new();
        storage
            .insert_user(&storage::User {
                id: user,
                name: "owner".into(),
                created_at_ms: 1,
            })
            .unwrap();
        storage
            .insert_bot(&storage::Bot {
                id: protocol::BotId::new(),
                user_id: user,
                name: "Atlas".into(),
                config: serde_json::json!({"bot": {"name": "Atlas"}}),
                created_at_ms: 1,
            })
            .unwrap();
    }

    fn pass_file(dir: &Path, text: &str) -> PassphraseArgs {
        let path = dir.join("pass.txt");
        std::fs::write(&path, text).unwrap();
        PassphraseArgs {
            passphrase_file: Some(path),
            passphrase_env: None,
        }
    }

    fn make_backup(src: &Path, file: &Path, pass: &PassphraseArgs, include_secrets: bool) {
        let store = MemoryStore::new();
        if include_secrets {
            store.set("openai", "sk-test-secret-value").unwrap();
        }
        create_with_kdf(
            src,
            &store,
            &CreateCmd {
                file: file.to_owned(),
                passphrase: pass.clone(),
                include_secrets,
            },
            FAST,
            false,
            &mut Vec::new(),
        )
        .unwrap();
    }

    fn restore_cmd(file: &Path, pass: &PassphraseArgs) -> RestoreEncryptedCmd {
        RestoreEncryptedCmd {
            file: file.to_owned(),
            passphrase: pass.clone(),
            replace: false,
            allow_policy_relaxation: false,
            include_secrets: false,
        }
    }

    fn do_restore(
        dst: &Path,
        store: &dyn SecretStore,
        cmd: &RestoreEncryptedCmd,
    ) -> crate::Result<()> {
        // Port 1 is never a Praxi daemon.
        restore_encrypted(dst, 1, store, cmd, false, &mut Vec::new())
    }

    fn bot_names(dir: &Path) -> Vec<String> {
        let s = storage::Storage::open(dir).unwrap();
        let user = s.list_users().unwrap()[0].id;
        s.list_bots(user)
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect()
    }

    #[test]
    fn encrypted_backup_round_trips_and_hides_plaintext() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), &format!("{PASS}\n"));
        let file = work.path().join("praxi.bak");
        make_backup(src.path(), &file, &pass, false);
        assert!(!partial_path(&file).exists());
        let bytes = std::fs::read(&file).unwrap();
        // SQLite files start with a fixed magic string; the tar names appear in the clear.
        for marker in [
            &b"SQLite format 3"[..],
            b"bot.db",
            b"manifest.json",
            b"Atlas",
        ] {
            assert!(
                !bytes.windows(marker.len()).any(|w| w == marker),
                "plaintext leaked"
            );
        }
        assert!(
            create_with_kdf(
                src.path(),
                &MemoryStore::new(),
                &CreateCmd {
                    file: file.clone(),
                    passphrase: pass.clone(),
                    include_secrets: false
                },
                FAST,
                false,
                &mut Vec::new()
            )
            .is_err(),
            "an existing file is never overwritten"
        );

        let mut said = Vec::new();
        verify(
            &VerifyCmd {
                file: file.clone(),
                passphrase: pass.clone(),
            },
            false,
            &mut said,
        )
        .unwrap();
        assert!(String::from_utf8(said).unwrap().contains("is intact"));

        let dst = tempfile::tempdir().unwrap();
        do_restore(dst.path(), &MemoryStore::new(), &restore_cmd(&file, &pass)).unwrap();
        assert_eq!(bot_names(dst.path()), ["Atlas"]);
    }

    #[test]
    fn wrong_passphrase_never_touches_an_existing_installation() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let file = work.path().join("praxi.bak");
        make_backup(src.path(), &file, &pass, false);
        let other = pass_file(work.path(), "a different passphrase");
        let mut cmd = restore_cmd(&file, &other);
        cmd.replace = true;
        let before = std::fs::read(src.path().join("bot.db")).unwrap();
        let err = do_restore(src.path(), &MemoryStore::new(), &cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("wrong passphrase"), "{err}");
        assert_eq!(std::fs::read(src.path().join("bot.db")).unwrap(), before);
        assert!(
            !std::fs::read_dir(src.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("backup-")),
            "no replacement backup folder: nothing was swapped"
        );
    }

    #[test]
    fn tampered_and_truncated_backups_fail_verify_and_restore() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let file = work.path().join("praxi.bak");
        make_backup(src.path(), &file, &pass, false);
        let good = std::fs::read(&file).unwrap();
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for at in [3, 25, good.len() / 2, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 0x01;
            cases.push(bad);
        }
        cases.push(good[..good.len() - 1].to_vec());
        cases.push(good[..good.len() / 2].to_vec());
        for (i, bad) in cases.iter().enumerate() {
            let broken = work.path().join(format!("broken-{i}.bak"));
            std::fs::write(&broken, bad).unwrap();
            assert!(inspect(&broken, PASS).is_err(), "case {i} passed verify");
            let dst = tempfile::tempdir().unwrap();
            assert!(
                do_restore(
                    dst.path(),
                    &MemoryStore::new(),
                    &restore_cmd(&broken, &pass)
                )
                .is_err(),
                "case {i} restored"
            );
            assert!(!dst.path().join("bot.db").exists(), "case {i} wrote state");
        }
    }

    #[test]
    fn a_large_file_is_streamed_through_many_chunks() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let notes = src.path().join("brain");
        std::fs::create_dir_all(&notes).unwrap();
        // 6 MiB of non-repeating data: ~96 chunks.
        let big: Vec<u8> = (0..6 * 1024 * 1024u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(notes.join("big.md"), &big).unwrap();
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let file = work.path().join("praxi.bak");
        make_backup(src.path(), &file, &pass, false);
        assert!(std::fs::metadata(&file).unwrap().len() > big.len() as u64);
        let dst = tempfile::tempdir().unwrap();
        do_restore(dst.path(), &MemoryStore::new(), &restore_cmd(&file, &pass)).unwrap();
        assert_eq!(std::fs::read(dst.path().join("brain/big.md")).unwrap(), big);
    }

    #[test]
    fn restore_refuses_while_a_daemon_holds_the_lock() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let file = work.path().join("praxi.bak");
        make_backup(src.path(), &file, &pass, false);
        let dst = tempfile::tempdir().unwrap();
        let fake_daemon = hold_daemon_lock(dst.path()).expect("lock");
        let err = do_restore(dst.path(), &MemoryStore::new(), &restore_cmd(&file, &pass))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot restore while a daemon"), "{err}");
        assert!(!dst.path().join("bot.db").exists());
        drop(fake_daemon);
        do_restore(dst.path(), &MemoryStore::new(), &restore_cmd(&file, &pass)).unwrap();
        assert_eq!(bot_names(dst.path()), ["Atlas"]);
    }

    #[test]
    fn secrets_are_only_restored_with_the_explicit_flag() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let plain = work.path().join("plain.bak");
        make_backup(src.path(), &plain, &pass, false);
        let secret = work.path().join("secret.bak");
        make_backup(src.path(), &secret, &pass, true);
        assert!(
            inspect(&plain, PASS)
                .unwrap()
                .manifest
                .secret_names
                .is_empty()
        );
        assert_eq!(
            inspect(&secret, PASS).unwrap().manifest.secret_names,
            ["openai"]
        );

        let skipped_store = MemoryStore::new();
        let dst = tempfile::tempdir().unwrap();
        do_restore(dst.path(), &skipped_store, &restore_cmd(&secret, &pass)).unwrap();
        assert!(skipped_store.get("openai").unwrap().is_none());

        let written_store = MemoryStore::new();
        let dst = tempfile::tempdir().unwrap();
        let mut cmd = restore_cmd(&secret, &pass);
        cmd.include_secrets = true;
        do_restore(dst.path(), &written_store, &cmd).unwrap();
        assert_eq!(
            written_store.get("openai").unwrap().unwrap().expose(),
            "sk-test-secret-value"
        );
    }

    #[test]
    fn passphrase_sources_reject_empty_and_short_values() {
        let work = tempfile::tempdir().unwrap();
        let empty = pass_file(work.path(), "\n");
        assert!(read_passphrase(&empty, false).is_err());
        let short = pass_file(work.path(), "short");
        assert!(read_passphrase(&short, true).is_err());
        assert!(read_passphrase(&short, false).is_ok());
        let crlf = pass_file(work.path(), "long enough pass\r\nsecond line");
        assert_eq!(&**read_passphrase(&crlf, true).unwrap(), "long enough pass");
        let missing = PassphraseArgs {
            passphrase_file: None,
            passphrase_env: Some("PRAXI_TEST_UNSET_PASSPHRASE_VAR".into()),
        };
        assert!(read_passphrase(&missing, false).is_err());
    }

    #[test]
    fn backup_of_an_empty_directory_says_so_and_creates_no_file() {
        let empty = tempfile::tempdir().unwrap();
        let file = empty.path().join("out.bak");
        let err = create_with_kdf(
            empty.path(),
            &MemoryStore::new(),
            &CreateCmd {
                file: file.clone(),
                passphrase: PassphraseArgs::default(),
                include_secrets: false,
            },
            FAST,
            false,
            &mut Vec::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("nothing to back up"), "{err}");
        assert!(!file.exists());
    }

    #[test]
    fn restore_refuses_a_populated_install_without_replace() {
        let src = tempfile::tempdir().unwrap();
        data_with_bot(src.path());
        let work = tempfile::tempdir().unwrap();
        let pass = pass_file(work.path(), PASS);
        let file = work.path().join("b.bak");
        make_backup(src.path(), &file, &pass, false);
        let err = do_restore(src.path(), &MemoryStore::new(), &restore_cmd(&file, &pass))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--replace"), "{err}");
    }
}
