//! Streaming state bundles. Stop the target daemon before importing.
//!
//! See the crate README for the format, trust model and swap limitations.

mod install;
mod policy;
mod transfer;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

pub use policy::POLICY_SCHEMA_VERSION;
pub use transfer::{EXCLUSIONS, INCLUDED_DIRECTORIES, INCLUDED_FILES};

/// Supported archive format.
pub const FORMAT_VERSION: u32 = 2;

/// An error during either direction of transfer. Errors never contain values.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("target contains bots or chats; replace is required")]
    TargetNotEmpty,
    #[error(
        "imported global policy may grant more permissions; allow_policy_relaxation is required"
    )]
    PolicyRelaxation,
    #[error("invalid bundle: {0}")]
    Invalid(String),
    #[error("database {db} has schema version {found}, newer than supported {supported}")]
    IncompatibleSchema {
        db: String,
        found: u32,
        supported: u32,
    },
    #[error("invalid database {db}: {reason}")]
    InvalidDatabase { db: String, reason: String },
    #[error("bundle exceeds configured size limit")]
    SizeLimit,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage: {0}")]
    Storage(#[from] storage::StorageError),
    #[error("secret store: {0}")]
    Secret(#[from] secret_store::SecretError),
    #[error("invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("rollback failed; manual recovery required: {0}")]
    Recovery(String),
}

/// Result of a bundle operation.
pub type Result<T> = std::result::Result<T, ImportError>;

/// A payload entry, in archive order. The manifest does not hash itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Final entry in every bundle. Timestamps are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub schema_version: u32,
    /// Per-database source schema versions. Older bundles omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_schemas: Option<BTreeMap<String, u32>>,
    pub app_version: String,
    pub epoch: u64,
    pub created_at: u64,
    pub source_key: Option<String>,
    pub secret_names: Vec<String>,
    pub files: Vec<FileRecord>,
}

/// Select secrets explicitly; the safe default is none.
#[derive(Debug, Clone, Default)]
pub enum SecretSelection {
    #[default]
    None,
    All,
    Names(Vec<String>),
}

/// Transfer phase for progress consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Export,
    Receive,
    Verify,
    Migrate,
    Swap,
    Complete,
}

/// Cumulative archive bytes transferred, and current phase.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub bytes: u64,
    pub phase: Phase,
}

/// Called synchronously; callbacks must not panic or block.
pub type ProgressCallback = Arc<dyn Fn(Progress) + Send + Sync>;

/// Export metadata and secret selection.
#[derive(Clone)]
pub struct ExportOptions {
    pub secrets: SecretSelection,
    pub app_version: String,
    pub epoch: u64,
    pub source_key: Option<String>,
    pub progress: Option<ProgressCallback>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            secrets: SecretSelection::None,
            app_version: env!("CARGO_PKG_VERSION").into(),
            epoch: 0,
            source_key: None,
            progress: None,
        }
    }
}

/// Import limits and destination host/permission policy. Host mode defaults to
/// standalone, state to standby; activation and epoch advancement belong to handover.
#[derive(Clone)]
pub struct ImportOptions {
    pub replace: bool,
    /// Explicitly approve replacing a valid destination policy with a more
    /// permissive one. Does not bypass missing, malformed, or unknown policies.
    pub allow_policy_relaxation: bool,
    pub max_bytes: u64,
    pub max_entry_bytes: u64,
    pub host_mode: String,
    pub host_state: String,
    pub progress: Option<ProgressCallback>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            replace: false,
            allow_policy_relaxation: false,
            max_bytes: 2 * 1024 * 1024 * 1024,
            max_entry_bytes: 1024 * 1024 * 1024,
            host_mode: "standalone".into(),
            host_state: "standby".into(),
            progress: None,
        }
    }
}

/// Successfully installed state and names of secrets written.
#[derive(Debug)]
pub struct ImportOutcome {
    pub manifest: Manifest,
    pub backup_dir: Option<PathBuf>,
    pub secrets_written: Vec<String>,
}

fn invalid(reason: &str) -> ImportError {
    ImportError::Invalid(reason.into())
}

fn progress(cb: Option<&ProgressCallback>, bytes: u64, phase: Phase) {
    if let Some(cb) = cb {
        cb(Progress { bytes, phase });
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Write a single-pass tar stream with a final manifest. Live databases use
/// SQLite online backups; selected plaintext secrets never touch disk.
///
/// # Errors
/// Returns errors for I/O, invalid filesystem entries/policies, or unavailable secrets.
pub async fn export(
    data_dir: impl AsRef<Path>,
    store: &dyn SecretStore,
    opts: ExportOptions,
    mut sink: impl AsyncWrite + Unpin,
) -> Result<Manifest> {
    let data_dir = data_dir.as_ref();
    let mut manifest = Manifest {
        format_version: FORMAT_VERSION,
        schema_version: 0,
        db_schemas: Some(BTreeMap::new()),
        app_version: opts.app_version,
        epoch: opts.epoch,
        created_at: now(),
        source_key: opts.source_key,
        secret_names: Vec::new(),
        files: Vec::new(),
    };
    let mut bytes = 0;
    transfer::write_files(
        data_dir,
        &mut sink,
        &mut manifest,
        &mut bytes,
        opts.progress.as_ref(),
    )
    .await?;
    let mut names = match opts.secrets {
        SecretSelection::None => Vec::new(),
        SecretSelection::All => store.list_names()?,
        SecretSelection::Names(names) => names,
    };
    names.sort();
    names.dedup();
    if !names.is_empty() {
        let mut values = BTreeMap::new();
        for name in &names {
            secret_store::validate_name(name)?;
            let value = store
                .get(name)?
                .ok_or_else(|| secret_store::SecretError::NotFound(name.clone()))?;
            values.insert(name.clone(), Zeroizing::new(value.expose().to_owned()));
        }
        let json = Zeroizing::new(serde_json::to_vec(&values)?);
        manifest.files.push(
            transfer::write_entry(
                &mut sink,
                "secrets.json",
                json.len() as u64,
                &mut json.as_slice(),
                &mut bytes,
                opts.progress.as_ref(),
            )
            .await?,
        );
        manifest.secret_names = names;
    }
    let json = serde_json::to_vec(&manifest)?;
    transfer::write_entry(
        &mut sink,
        "manifest.json",
        json.len() as u64,
        &mut json.as_slice(),
        &mut bytes,
        opts.progress.as_ref(),
    )
    .await?;
    sink.write_all(&[0; 1024]).await?;
    sink.flush().await?;
    progress(opts.progress.as_ref(), bytes + 1024, Phase::Complete);
    Ok(manifest)
}

/// Validate and stage a bundle, migrate its database and install it. The target
/// daemon must be stopped, with all SQLite handles closed. Staging is removed
/// on error and cancellation. Secrets are written only after validation.
///
/// # Errors
/// Rejects unsafe archives, incompatible schemas, unapproved policy relaxations,
/// limits and nonempty targets.
pub async fn import(
    data_dir: impl AsRef<Path>,
    store: &dyn SecretStore,
    source: impl AsyncRead + Unpin,
    opts: ImportOptions,
) -> Result<ImportOutcome> {
    let data_dir = data_dir.as_ref();
    install::check_target(data_dir, opts.replace)?;
    std::fs::create_dir_all(data_dir)?;
    let staging = install::Staging::create(data_dir)?;
    let (manifest, secrets, bytes) = transfer::receive(&staging.0, source, &opts).await?;
    progress(opts.progress.as_ref(), bytes, Phase::Verify);
    let incoming_policy = policy::imported(&staging.0)?;
    policy::check_destination(data_dir, &incoming_policy, opts.allow_policy_relaxation)?;
    install::prepare(&staging.0, &manifest, &opts)?;
    progress(opts.progress.as_ref(), bytes, Phase::Migrate);
    // Recheck after the potentially long receive (callers must still serialize imports).
    install::check_target(data_dir, opts.replace)?;
    progress(opts.progress.as_ref(), bytes, Phase::Swap);
    // The receive/migrations and even a progress callback can take time. Check
    // the current destination ceiling immediately before any live-state write.
    // commit checks the policy file it actually renames, so a write that lands
    // after this call still cannot loosen the captured ceiling.
    policy::check_destination(data_dir, &incoming_policy, opts.allow_policy_relaxation)?;
    // No await points after writes start: cancellation cannot interrupt rollback.
    let backup_dir = install::commit(
        data_dir,
        &staging.0,
        store,
        &secrets,
        &incoming_policy,
        opts.allow_policy_relaxation,
    )?;
    progress(opts.progress.as_ref(), bytes, Phase::Complete);
    Ok(ImportOutcome {
        secrets_written: manifest.secret_names.clone(),
        manifest,
        backup_dir,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod policy_tests;
