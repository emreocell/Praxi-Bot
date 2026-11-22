//! Diagnostics shared by `bot doctor` and `GET /v1/doctor`.
//!
//! Every check is a plain function over injected inputs (data dir, catalog,
//! secret store, CLI detection result, ...) returning a typed [`Check`], so
//! the CLI and the daemon render the same data and tests drive the checks with
//! temp dirs and fakes.  Checks only read: the one exception is the Unicode
//! path probe, which creates, reads and deletes its own probe file.
//!
//! A [`Repair`] is a closed, allow-listed enum: never a command line.  The
//! daemon runs one only on an explicit `POST /v1/doctor/repair/{id}` (a click
//! in Settings) and audit-logs it.
//!
//! Secrets are never read into the output: only their presence is checked,
//! and support reports go through [`redact_report`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use provider_catalog::{
    Auth, Catalog, ProviderConfig, ProviderEntry, ProviderSettings, config_path,
};
use secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

/// Free space below which the disk check warns.
pub const DISK_WARN_BYTES: u64 = 1 << 30;
/// Free space below which the disk check fails.
pub const DISK_FAIL_BYTES: u64 = 100 << 20;
/// Age in days after which the curated model catalog triggers a warning.
pub const CATALOG_STALE_DAYS: u32 = provider_catalog::DEFAULT_STALE_AFTER_DAYS;
/// A data file written further than this in the future means the clock went back.
pub const CLOCK_SKEW_TOLERANCE: Duration = Duration::from_secs(5 * 60);
/// File name of the Unicode path probe (Turkish letters on purpose: the most
/// common non-ASCII user names and folders of this project's users).
pub const UNICODE_PROBE_NAME: &str = "praxi-tanılama-ğüşiöç-İĞÜŞÖÇ.txt";
/// Earliest plausible wall-clock time (2025-01-01T00:00:00Z).
const CLOCK_FLOOR_SECS: u64 = 1_735_689_600;

/// Result class of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Fine.
    Ok,
    /// Works, but worth a look (optional part missing, low space).
    Warn,
    /// Broken; the bot will not work properly.
    Fail,
    /// Not applicable here (not configured, or not available on this
    /// platform).  Neutral, never an error.
    Skipped,
}

impl Status {
    /// Table symbol.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "\u{2713}",
            Self::Warn => "!",
            Self::Fail => "\u{2717}",
            Self::Skipped => "-",
        }
    }

    /// Lower-case word (`ok`, `warn`, `fail`, `skipped`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

/// An allow-listed, one-click repair.  Deliberately closed: nothing here runs
/// a shell command, and regenerating the API token is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Repair {
    /// Create the missing data directory.
    CreateDataDir,
    /// Rebuild the providers from `providers.toml` and the secret store.
    ReloadProviders,
    /// Restart the shared browser session (only when the embedder supports it).
    RestartBrowser,
}

impl Repair {
    /// Every repair.
    pub const ALL: [Self; 3] = [
        Self::CreateDataDir,
        Self::ReloadProviders,
        Self::RestartBrowser,
    ];

    /// Stable id used in `POST /v1/doctor/repair/{id}`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::CreateDataDir => "create_data_dir",
            Self::ReloadProviders => "reload_providers",
            Self::RestartBrowser => "restart_browser",
        }
    }

    /// The repair with this exact id (`None` for anything else).
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.id() == id)
    }
}

/// One diagnostic result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Check {
    /// Stable id (`data_dir`, `providers`, ...).
    pub id: String,
    /// Short human name (`data dir`, `providers`, ...), as `bot doctor` prints it.
    pub title: String,
    /// Result class.
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// How to fix it by hand (for warn / fail).
    pub hint: Option<String>,
    /// The one-click repair, when one is allowed for this finding.
    pub repair: Option<Repair>,
}

impl Check {
    /// A check result.
    #[must_use]
    pub fn new(id: &str, title: &str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            status,
            detail: detail.into(),
            hint: None,
            repair: None,
        }
    }

    /// `Ok`.
    #[must_use]
    pub fn ok(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Ok, detail)
    }

    /// `Warn` with a hint.
    #[must_use]
    pub fn warn(id: &str, title: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(id, title, Status::Warn, detail).hint(hint)
    }

    /// `Fail` with a hint.
    #[must_use]
    pub fn fail(id: &str, title: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(id, title, Status::Fail, detail).hint(hint)
    }

    /// `Skipped` (neutral).
    #[must_use]
    pub fn skipped(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Skipped, detail)
    }

    /// Attach a manual fix hint.
    #[must_use]
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }

    /// Attach a one-click repair.
    #[must_use]
    pub fn repair(mut self, r: Repair) -> Self {
        self.repair = Some(r);
        self
    }
}

// ------------------------------------------------------------------ checks

/// `host ownership`: whether `host.toml` is valid and this installation runs
/// its components.
#[must_use]
pub fn check_host(data_dir: &Path) -> Check {
    const ID: &str = "host_ownership";
    const N: &str = "host ownership";
    match crate::host::HostConfig::load(data_dir) {
        Ok(config) => {
            let detail = format!(
                "mode={:?}, state={:?}, epoch={}, leader={}",
                config.mode,
                config.state,
                config.epoch,
                config.leader.as_deref().unwrap_or("local")
            );
            if config.runs_components() {
                Check::ok(ID, N, detail)
            } else {
                Check::warn(
                    ID,
                    N,
                    detail,
                    "this installation is idle; use bot host status or explicitly activate --force for emergency local operation",
                )
            }
        }
        Err(e) => Check::fail(
            ID,
            N,
            e.to_string(),
            "fix host.toml; the daemon refuses invalid ownership state",
        ),
    }
}

/// One paired device as the running daemon sees it (the data of
/// `GET /v1/host`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPeer {
    /// Full peer key.
    pub key: String,
    /// Pairing role (`desktop`, `phone`, `node`, `host`).
    pub role: String,
    /// Connected right now.
    pub connected: bool,
    /// Seconds since the device was last heard from (`None` = never).
    pub last_seen_secs: Option<u64>,
}

/// Live peer state the link check looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRuntime {
    /// Not available here (`bot doctor` without the daemon's live state).
    Unknown,
    /// The daemon runs without peer networking.
    NetworkDisabled,
    /// The daemon's paired devices with their live connection state.
    Network(Vec<LinkPeer>),
}

fn ago(secs: Option<u64>) -> String {
    match secs {
        None => "never seen".into(),
        Some(s) if s < 120 => format!("last seen {s} s ago"),
        Some(s) if s < 7200 => format!("last seen {} min ago", s / 60),
        Some(s) => format!("last seen {} h ago", s / 3600),
    }
}

const NOT_VERIFIED_HINT: &str =
    "open Settings > Diagnostics while the daemon runs, or run `bot host status`";
const NETWORK_OFF_HINT: &str = "start the daemon with peer networking enabled (`bot serve --peer`), or switch back to standalone";

/// `peer link` of the always-on host: its endpoint must run.
fn host_link(config: &crate::host::HostConfig, live: &LinkRuntime) -> Check {
    const ID: &str = "peer_link";
    const N: &str = "peer link";
    let role = format!(
        "this machine is the always-on host (state {:?}, epoch {})",
        config.state, config.epoch
    );
    match live {
        LinkRuntime::Unknown => Check::warn(
            ID,
            N,
            format!("{role}; live peer connections not verified"),
            NOT_VERIFIED_HINT,
        ),
        LinkRuntime::NetworkDisabled => Check::fail(
            ID,
            N,
            format!("{role}, but peer networking is off: no device can reach it"),
            NETWORK_OFF_HINT,
        ),
        LinkRuntime::Network(peers) => {
            let connected = peers.iter().filter(|p| p.connected).count();
            Check::ok(
                ID,
                N,
                format!(
                    "{role}; {} paired device(s), {connected} connected",
                    peers.len()
                ),
            )
        }
    }
}

/// `peer link`: the host / client pairing this installation is configured
/// for, checked against the daemon's live connection state.  Configured but
/// not connected is a finding; without live state the link is reported as not
/// verified (never as healthy).  Skipped when it runs on its own.
#[must_use]
pub fn check_peer_link(data_dir: &Path, live: &LinkRuntime) -> Check {
    use crate::host::{HostMode, HostState};
    const ID: &str = "peer_link";
    const N: &str = "peer link";
    let Ok(config) = crate::host::HostConfig::load(data_dir) else {
        return Check::skipped(ID, N, "host.toml is invalid; see the host ownership check");
    };
    let leader_key = config.leader.clone();
    let leader = leader_key.as_deref().map(short_key);
    match (config.mode, leader) {
        (HostMode::Standalone, None) => {
            Check::skipped(ID, N, "no peer or host link configured (optional)")
        }
        (HostMode::Standalone, Some(l)) => {
            Check::ok(ID, N, format!("standalone; last known leader {l}"))
        }
        (HostMode::Host, _) => host_link(&config, live),
        (HostMode::Client, Some(l)) => {
            let standby = if config.state == HostState::Standby {
                " (this machine is standby)"
            } else {
                ""
            };
            match live {
                LinkRuntime::Unknown => Check::warn(
                    ID,
                    N,
                    format!("configured as client of host {l}{standby}; connection not verified"),
                    NOT_VERIFIED_HINT,
                ),
                LinkRuntime::NetworkDisabled => Check::fail(
                    ID,
                    N,
                    format!("configured as client of host {l}, but peer networking is off"),
                    NETWORK_OFF_HINT,
                ),
                LinkRuntime::Network(peers) => {
                    match peers.iter().find(|p| Some(&p.key) == leader_key.as_ref()) {
                        None => Check::fail(
                            ID,
                            N,
                            format!(
                                "configured as client of host {l}, but that host is no longer paired"
                            ),
                            "pair this device with the host again from the host's Settings",
                        ),
                        Some(p) if p.connected => {
                            Check::ok(ID, N, format!("connected to host {l}{standby}"))
                        }
                        Some(p) => Check::fail(
                            ID,
                            N,
                            format!(
                                "configured as client of host {l}, but not connected ({})",
                                ago(p.last_seen_secs)
                            ),
                            "check that the host (VPS) is running and online, then run Diagnostics again",
                        ),
                    }
                }
            }
        }
        (HostMode::Client, None) => Check::warn(
            ID,
            N,
            "client mode, but no host is paired",
            "pair this device with the host from the host's Settings, or switch back to standalone",
        ),
    }
}

/// First 8 characters of a peer key, enough to tell devices apart.
fn short_key(key: &str) -> String {
    let short: String = key.chars().take(8).collect();
    if short.len() < key.len() {
        format!("{short}\u{2026}")
    } else {
        short
    }
}

/// An unpredictable probe file name: `prefix`, 32 hex digits, `suffix`.
fn probe_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}{suffix}", uuid::Uuid::now_v7().simple())
}

/// [`exclusive_probe_with`] without an extra step.
pub(crate) fn exclusive_probe(dir: &Path, name: &str, payload: &[u8]) -> std::io::Result<Vec<u8>> {
    exclusive_probe_with(dir, name, payload, || Ok(()))
}

/// Create `dir/name` exclusively, write `payload`, run `between`, read the
/// file back and delete it.
///
/// The file is created with `create_new` (`O_CREAT | O_EXCL` /
/// `CREATE_NEW`): an existing file or symlink of that name is never opened,
/// truncated or followed, and the call then fails without touching it.  Only a
/// file this call created is deleted, also when a later step fails.
pub(crate) fn exclusive_probe_with(
    dir: &Path,
    name: &str,
    payload: &[u8],
    between: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<Vec<u8>> {
    use std::io::Write as _;
    let path = dir.join(name);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    // From here on the file is ours: always remove it.
    let result = (|| {
        (&file).write_all(payload)?;
        (&file).flush()?;
        drop(file);
        between()?;
        std::fs::read(&path)
    })();
    let removed = std::fs::remove_file(&path);
    let back = result?;
    removed?;
    Ok(back)
}

/// `data dir`: exists, is a directory, is writable, and has enough free disk
/// space.  Offers [`Repair::CreateDataDir`] when it is missing.
#[must_use]
pub fn check_data_dir(dir: &Path) -> Check {
    const ID: &str = "data_dir";
    const N: &str = "data dir";
    if !dir.exists() {
        if dir
            .ancestors()
            .skip(1)
            .find(|p| !p.as_os_str().is_empty() && p.exists())
            .is_some_and(|p| !p.is_dir())
        {
            return Check::fail(
                ID,
                N,
                format!(
                    "{} cannot be created because a parent path is not a directory",
                    dir.display()
                ),
                "remove the blocking file or pass another --data-dir",
            );
        }
        return Check::warn(
            ID,
            N,
            format!("{} does not exist yet", dir.display()),
            "it is created by `bot setup` or `bot serve`",
        )
        .repair(Repair::CreateDataDir);
    }
    if !dir.is_dir() {
        return Check::fail(
            ID,
            N,
            format!("{} is not a directory", dir.display()),
            "remove the file or pass another --data-dir",
        );
    }
    if let Err(e) = exclusive_probe(dir, &probe_name(".doctor-probe-", ""), b"ok") {
        return Check::fail(
            ID,
            N,
            format!("{} is not writable: {e}", dir.display()),
            "fix the folder permissions or pass another --data-dir",
        );
    }
    data_dir_with_free_space(dir, fs4::available_space(dir).map_err(|e| e.to_string()))
}

/// Finish [`check_data_dir`] once `dir` is confirmed to exist and be writable,
/// evaluating the measured free space against [`DISK_WARN_BYTES`] and
/// [`DISK_FAIL_BYTES`].
#[must_use]
pub fn data_dir_with_free_space(dir: &Path, free: Result<u64, String>) -> Check {
    const ID: &str = "data_dir";
    const N: &str = "data dir";
    match free {
        Ok(free) => {
            let detail = format!("{} (writable, {} MiB free)", dir.display(), free >> 20);
            if free < DISK_FAIL_BYTES {
                Check::fail(
                    ID,
                    N,
                    detail,
                    "free some disk space; runs and logs cannot be saved",
                )
            } else if free < DISK_WARN_BYTES {
                Check::warn(ID, N, detail, "less than 1 GiB left; free some space")
            } else {
                Check::ok(ID, N, detail)
            }
        }
        Err(e) => Check::warn(
            ID,
            N,
            format!("{} (writable; cannot read free space: {e})", dir.display()),
            "ignore if unsure",
        ),
    }
}

/// `unicode paths`: create, read back and delete a file whose name has
/// Turkish letters, inside the data dir (catches code pages and file systems
/// that mangle non-ASCII names).
#[must_use]
pub fn check_unicode_path(dir: &Path) -> Check {
    const ID: &str = "unicode_path";
    const N: &str = "unicode paths";
    if !dir.is_dir() {
        return Check::skipped(ID, N, "data dir does not exist yet");
    }
    let name = probe_name("", &format!("-{UNICODE_PROBE_NAME}"));
    let payload = "Praxi tanılama: ğüşiöç İĞÜŞÖÇ";
    let mut listed = false;
    let result = exclusive_probe_with(dir, &name, payload.as_bytes(), || {
        listed = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_str() == Some(name.as_str()));
        Ok(())
    })
    .and_then(|back| {
        if back == payload.as_bytes() {
            Ok(listed)
        } else {
            Err(std::io::Error::other("content changed on read-back"))
        }
    });
    match result {
        Ok(true) => Check::ok(ID, N, "non-ASCII file names work in the data dir"),
        // Readable by name, but listed in another form (e.g. Unicode
        // normalisation of the file system): works, worth knowing.
        Ok(false) => Check::warn(
            ID,
            N,
            "non-ASCII file names work, but the folder lists them in a different form",
            "harmless for the bot; keep file names ASCII if another tool cannot find them",
        ),
        Err(e) => Check::fail(
            ID,
            N,
            format!("non-ASCII file name probe failed: {e}"),
            "move the data dir to a folder on an NTFS/ext4/APFS disk, or set BOT_DATA_DIR to an ASCII-only path",
        ),
    }
}

/// `config`: `providers.toml` parses.
#[must_use]
pub fn check_config(dir: &Path) -> Check {
    const ID: &str = "config";
    const N: &str = "config";
    let path = config_path(dir);
    if !path.exists() {
        return Check::warn(
            ID,
            N,
            "providers.toml not found",
            "run `bot setup` to add a provider",
        );
    }
    match ProviderConfig::load(&path) {
        Ok(c) if !c.quarantined.is_empty() => Check::fail(
            ID,
            N,
            format!(
                "{} provider(s) disabled because of reserved secret references",
                c.quarantined.len()
            ),
            "remove the reserved key reference in providers.toml and reconnect each disabled provider",
        ),
        Ok(c) => Check::ok(
            ID,
            N,
            format!("providers.toml parses ({} provider(s))", c.providers.len()),
        ),
        Err(e) => Check::fail(
            ID,
            N,
            format!("providers.toml: {e}"),
            "fix the file by hand or move it away and run `bot setup`",
        ),
    }
}

/// The opened secret store and a label of its backend, or why it failed.
pub type StoreRef<'a> = Result<(&'a dyn SecretStore, String), String>;

/// `secret backend`: the store opens and lists.
#[must_use]
pub fn check_secrets(store: &StoreRef<'_>) -> Check {
    const ID: &str = "secret_backend";
    const N: &str = "secret backend";
    match store {
        Ok((store, kind)) => match store.list_names() {
            Ok(names) => Check::ok(ID, N, format!("{kind} ({} secret(s))", names.len())),
            Err(e) => Check::fail(
                ID,
                N,
                format!("{kind}: {e}"),
                "check BOT_SECRETS_BACKEND / BOT_SECRETS_PASSPHRASE",
            ),
        },
        Err(e) => Check::fail(
            ID,
            N,
            e.clone(),
            "set BOT_SECRETS_PASSPHRASE for the encrypted file, or use BOT_SECRETS_BACKEND=keyring",
        ),
    }
}

/// Online test of one provider: the number of models it answered with.
pub type OnlineProbe<'a> =
    &'a dyn Fn(&ProviderSettings, &ProviderEntry, &dyn SecretStore) -> Result<usize, String>;

/// Everything the provider check looks at.
pub struct ProviderInputs<'a> {
    /// Data directory (`providers.toml`).
    pub data_dir: &'a Path,
    /// Loaded catalog, or why loading failed.
    pub catalog: Result<&'a Catalog, String>,
    /// Secret store.
    pub store: &'a StoreRef<'a>,
    /// Detected coding CLIs (`None` = not probed).
    pub detected: Option<&'a [cli_provider::DetectedCli]>,
    /// Test connections over the network (`bot doctor --online`).
    pub online: Option<OnlineProbe<'a>>,
}

/// `providers`: every enabled provider has its key (or, for a coding CLI
/// provider, a signed-in CLI).
#[must_use]
pub fn check_providers(inp: &ProviderInputs<'_>) -> Check {
    const ID: &str = "providers";
    const N: &str = "providers";
    let Ok(cfg) = ProviderConfig::load(&config_path(inp.data_dir)) else {
        return Check::warn(ID, N, "no readable provider config", "see the config check");
    };
    let enabled: Vec<_> = cfg.providers.iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        return Check::fail(
            ID,
            N,
            "setup required: no enabled provider",
            "run `bot setup` or `bot providers add`",
        );
    }
    let mut problems = Vec::new();
    let mut unverified = Vec::new();
    let mut notes = Vec::new();
    for p in &enabled {
        if p.cli.is_some() {
            match cli_state(&p.id, inp.detected) {
                CliState::Ready(note) => notes.push(note),
                CliState::Unverified(note) => unverified.push(note),
                CliState::Problem(problem) => problems.push(problem),
            }
            continue;
        }
        let entry = p.custom.clone().or_else(|| {
            inp.catalog
                .as_ref()
                .ok()
                .and_then(|c| c.get(&p.id).cloned())
        });
        let Some(entry) = entry else {
            problems.push(format!("{}: not in the catalog", p.id));
            continue;
        };
        let key_ok = match (&p.key, &entry.auth) {
            (Some(r), _) => match inp.store {
                Ok((s, _)) => s.get(r.name()).ok().flatten().is_some(),
                Err(_) => false,
            },
            (None, Auth::None) => true,
            (None, Auth::ApiKeyEnv(var)) => std::env::var(var).is_ok_and(|v| !v.is_empty()),
        };
        if !key_ok {
            problems.push(format!("{}: API key not available", p.id));
            continue;
        }
        if let Some(probe) = inp.online {
            match inp.store {
                Ok((s, _)) => match probe(p, &entry, *s) {
                    Ok(n) => notes.push(format!("{}: reachable ({n} models)", p.id)),
                    Err(e) => problems.push(format!("{}: {e}", p.id)),
                },
                Err(_) => problems.push(format!("{}: secret store unavailable", p.id)),
            }
        } else {
            notes.push(format!("{}: configured", p.id));
        }
    }
    let detail = notes
        .iter()
        .chain(unverified.iter())
        .chain(problems.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if problems.is_empty() && !unverified.is_empty() {
        Check::warn(
            ID,
            N,
            detail,
            "the coding CLI sign-in could not be confirmed: run Diagnostics again, or open a terminal and sign in to the CLI once (e.g. `claude`, `codex login`)",
        )
    } else if problems.is_empty() {
        let c = Check::ok(ID, N, detail);
        if inp.online.is_some() {
            c
        } else {
            c.hint("add --online to test the connections")
        }
    } else {
        Check::fail(
            ID,
            N,
            detail,
            "`bot secrets set provider.<id>.api_key` or `bot providers add <id>` to fix the key",
        )
    }
}

/// What the detection result says about an enabled `cli:<engine>` provider.
#[derive(Debug, PartialEq, Eq)]
enum CliState {
    /// Installed, runnable and affirmatively signed in.
    Ready(String),
    /// Not known to work: detection did not answer, or the sign-in state
    /// could not be read.  Reported as a warning, never as ready.
    Unverified(String),
    /// Known to be broken.
    Problem(String),
}

/// State of an enabled `cli:<engine>` provider from the detection result.
fn cli_state(provider_id: &str, detected: Option<&[cli_provider::DetectedCli]>) -> CliState {
    use cli_provider::AuthState;
    let engine = provider_id.strip_prefix("cli:").unwrap_or(provider_id);
    let Some(detected) = detected else {
        return CliState::Unverified(format!(
            "{provider_id}: configured, but the CLI was not checked (detection did not answer)"
        ));
    };
    match detected.iter().find(|d| d.id == engine) {
        Some(d) if !d.installed => {
            CliState::Problem(format!("{provider_id}: CLI is not installed"))
        }
        Some(d) if d.auth == AuthState::SignedOut => CliState::Problem(format!(
            "{provider_id}: CLI is installed but signed out; sign in with `{}`",
            sign_in_command(engine)
        )),
        Some(d) if !d.runnable => CliState::Problem(format!(
            "{provider_id}: CLI cannot run turns ({})",
            d.detail.as_deref().unwrap_or("not runnable")
        )),
        Some(d) if d.auth == AuthState::SignedIn => {
            CliState::Ready(format!("{provider_id}: CLI ready"))
        }
        Some(_) => CliState::Unverified(format!(
            "{provider_id}: CLI installed, but its sign-in could not be verified; if turns fail, sign in with `{}`",
            sign_in_command(engine)
        )),
        None => CliState::Problem(format!("{provider_id}: unknown coding CLI")),
    }
}

/// Terminal command that signs in to a coding CLI.
fn sign_in_command(engine: &str) -> &str {
    match engine {
        "codex" => "codex login",
        other => other,
    }
}

/// `cli accounts`: coding CLIs found on this machine (subscription use, ADR
/// 0028).  Optional: nothing found is neutral.
#[must_use]
pub fn check_cli_accounts(detected: Option<&[cli_provider::DetectedCli]>) -> Check {
    const ID: &str = "cli_accounts";
    const N: &str = "cli accounts";
    let Some(detected) = detected else {
        return Check::warn(
            ID,
            N,
            "coding CLI detection did not answer in time; CLI sign-ins were not verified",
            "run Diagnostics again; if it keeps timing out, run `bot doctor` in a terminal",
        );
    };
    let installed: Vec<_> = detected.iter().filter(|d| d.installed).collect();
    if installed.is_empty() {
        return Check::skipped(ID, N, "no coding CLI installed (optional)");
    }
    let mut ready = Vec::new();
    let mut signed_out = Vec::new();
    let mut unverified = Vec::new();
    let mut other = Vec::new();
    for d in installed {
        match d.auth {
            cli_provider::AuthState::SignedOut => signed_out.push(d.display_name.clone()),
            cli_provider::AuthState::SignedIn if d.runnable => ready.push(d.display_name.clone()),
            cli_provider::AuthState::Unknown if d.runnable => {
                unverified.push(d.display_name.clone());
            }
            _ => other.push(d.display_name.clone()),
        }
    }
    let mut parts = Vec::new();
    if !ready.is_empty() {
        parts.push(format!("ready: {}", ready.join(", ")));
    }
    if !signed_out.is_empty() {
        parts.push(format!("signed out: {}", signed_out.join(", ")));
    }
    if !unverified.is_empty() {
        parts.push(format!("sign-in not verified: {}", unverified.join(", ")));
    }
    if !other.is_empty() {
        parts.push(format!("detected only: {}", other.join(", ")));
    }
    let detail = parts.join("; ");
    if signed_out.is_empty() && unverified.is_empty() {
        Check::ok(ID, N, detail)
    } else if signed_out.is_empty() {
        Check::warn(
            ID,
            N,
            detail,
            "the CLI did not report whether it is signed in: open a terminal, run it once and sign in if asked (e.g. `claude`, `codex login`)",
        )
    } else {
        Check::warn(
            ID,
            N,
            detail,
            "open a terminal and sign in to the CLI once (e.g. `claude`, `codex login`)",
        )
    }
}

/// `model catalog`: the catalog loads and knows the default model.
#[must_use]
pub fn check_catalog(data_dir: &Path, catalog: &Result<&Catalog, String>) -> Check {
    const ID: &str = "model_catalog";
    const N: &str = "model catalog";
    let catalog = match catalog {
        Ok(c) => c,
        Err(e) => {
            return Check::fail(
                ID,
                N,
                e.clone(),
                "fix or unset BOT_CATALOG_DIR to use the built-in catalog",
            );
        }
    };
    if catalog.is_empty() {
        return Check::fail(ID, N, "catalog is empty", "unset BOT_CATALOG_DIR");
    }
    let n_models: usize = catalog.all().map(|e| e.models.len()).sum();
    let detail = format!("{} provider(s), {n_models} known model(s)", catalog.len());
    if let Ok(cfg) = ProviderConfig::load(&config_path(data_dir))
        && let Some(def) = cfg.effective_default()
    {
        match &def.default_model {
            None if def.cli.is_none() => {
                return Check::warn(
                    ID,
                    N,
                    format!("{detail}; default provider {} has no model", def.id),
                    "`bot providers default <id> --model <model>`",
                );
            }
            None => {}
            Some(m) => {
                let entry = def.custom.as_ref().or_else(|| catalog.get(&def.id));
                if entry.is_some_and(|e| !e.models.is_empty() && e.model(m).is_none()) {
                    return Check::warn(
                        ID,
                        N,
                        format!("{detail}; default model {m} is not in the catalog"),
                        "prices and limits are unknown for it; check the model id",
                    );
                }
            }
        }
    }
    Check::ok(ID, N, detail)
}

/// `catalog freshness`: warns when the checked-in curated model table has not
/// been reviewed for more than [`CATALOG_STALE_DAYS`] (90) days. Always a
/// warning, never a failure (Praxi works offline by design).
#[must_use]
pub fn check_catalog_freshness(now: SystemTime) -> Check {
    check_catalog_freshness_for(
        provider_catalog::embedded_metadata(),
        now,
        CATALOG_STALE_DAYS,
    )
}

/// Pure freshness check against an explicit metadata table and threshold
/// (used by tests with fixture tables and timestamps).
#[must_use]
pub fn check_catalog_freshness_for(
    table: &provider_catalog::CuratedModels,
    now: SystemTime,
    max_age_days: u32,
) -> Check {
    const ID: &str = "catalog_freshness";
    const N: &str = "catalog freshness";
    const HINT: &str =
        "run `bot catalog check` and review model_metadata.json against current provider docs";
    match provider_catalog::check_staleness(table, now, max_age_days) {
        Ok(report) if report.stale => Check::warn(
            ID,
            N,
            format!(
                "last reviewed {} ({} day(s) ago; older than {max_age_days} days)",
                report.oldest_reviewed, report.age_days
            ),
            HINT,
        ),
        Ok(report) if report.age_days < 0 => {
            Check::ok(ID, N, format!("last reviewed {}", report.oldest_reviewed))
        }
        Ok(report) => Check::ok(
            ID,
            N,
            format!(
                "last reviewed {} ({} day(s) ago)",
                report.oldest_reviewed, report.age_days
            ),
        ),
        Err(err) => Check::warn(ID, N, err, HINT),
    }
}

/// `provider reload`: providers enabled in `providers.toml` that the running
/// daemon has not loaded (added behind its back).  Offers
/// [`Repair::ReloadProviders`].
#[must_use]
pub fn check_providers_loaded(data_dir: &Path, active: &[String]) -> Check {
    const ID: &str = "providers_loaded";
    const N: &str = "loaded providers";
    let Ok(cfg) = ProviderConfig::load(&config_path(data_dir)) else {
        return Check::skipped(ID, N, "no readable provider config");
    };
    let missing: Vec<&str> = cfg
        .providers
        .iter()
        .filter(|p| p.enabled && !active.iter().any(|a| a == &p.id))
        .map(|p| p.id.as_str())
        .collect();
    if missing.is_empty() {
        if active.is_empty() {
            Check::skipped(ID, N, "no provider configured yet")
        } else {
            Check::ok(ID, N, format!("{} provider(s) loaded", active.len()))
        }
    } else {
        Check::warn(
            ID,
            N,
            format!("enabled but not loaded: {}", missing.join(", ")),
            "reload the providers; if that fails, see the providers check",
        )
        .repair(Repair::ReloadProviders)
    }
}

/// `process guard`: whether the daemon protected its own process (blocks
/// same-user children from reading its memory) at its last start.  Reads the
/// report the daemon recorded in the data dir; a missing report means the
/// daemon has not run yet, an old/other build, or an unsupported start path.
#[must_use]
pub fn check_process_guard(dir: &Path) -> Check {
    use process_guard::Outcome;
    const ID: &str = "process_guard";
    const N: &str = "process guard";
    let Some(report) = process_guard::load(dir) else {
        let msg = if dir.join(process_guard::REPORT_FILE).exists() {
            "report file is unreadable (torn or from another build); it is rewritten when the daemon starts"
        } else {
            "no report yet; it is written when the daemon starts"
        };
        return Check::skipped(ID, N, msg);
    };
    match report.outcome {
        Outcome::Applied => Check::ok(ID, N, format!("{} (pid {})", report.detail, report.pid)),
        Outcome::NotSupported | Outcome::Skipped => Check::skipped(ID, N, report.detail),
        Outcome::Failed => Check::warn(
            ID,
            N,
            format!("DEGRADED: {}", report.detail),
            "the daemon runs without memory protection against same-user child processes; restart it, and report this if it persists",
        ),
    }
}

/// `safe mode`: whether the daemon runs with tools, schedules, channels, MCP servers
/// and apps off. Safe mode is a recovery state, so it is a warning, never a failure.
#[must_use]
pub fn check_safe_mode(active: bool) -> Check {
    const ID: &str = "safe_mode";
    const N: &str = "safe mode";
    if active {
        Check::warn(
            ID,
            N,
            "ON: tools, schedules, channels, MCP servers and apps are off; chat answers without tools",
            "fix the problem, then restart the daemon without --safe (and without BOT_SAFE_MODE); nothing was deleted or changed",
        )
    } else {
        Check::ok(ID, N, "off")
    }
}

/// The environment switch for safe mode (`BOT_SAFE_MODE`): `1`, `true`, `on` or `yes`.
#[must_use]
pub fn safe_mode_from_env(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

/// `schedule tools`: whether `schedule.toml` is usable.  A broken one switches
/// the bots' schedule tools off (they never fall back to looser defaults).
#[must_use]
pub fn check_schedule_tools(dir: &Path) -> Check {
    const ID: &str = "schedule_tools";
    const N: &str = "schedule tools";
    match crate::load_schedule_limits(dir) {
        Ok(l) => Check::ok(
            ID,
            N,
            format!(
                "max {} per bot, minimum interval {} s",
                l.max_per_bot, l.min_interval_secs
            ),
        ),
        Err(e) => Check::fail(
            ID,
            N,
            format!("{e}; schedule tools are disabled until it is fixed"),
            format!("fix or delete {}", dir.join("schedule.toml").display()),
        ),
    }
}

/// `disk space` of the volume holding the data dir (or its nearest existing
/// parent).
#[must_use]
pub fn check_disk(dir: &Path) -> Check {
    let mut probe = dir;
    while !probe.exists() {
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p,
            _ => break,
        }
    }
    disk_check(fs4::available_space(probe).map_err(|e| e.to_string()))
}

/// The disk check over a measured free-space value.
#[must_use]
pub fn disk_check(free: Result<u64, String>) -> Check {
    const ID: &str = "disk_space";
    const N: &str = "disk space";
    match free {
        Ok(free) => {
            let detail = format!("{} MiB free", free >> 20);
            if free < DISK_FAIL_BYTES {
                Check::fail(
                    ID,
                    N,
                    detail,
                    "free some disk space; runs and logs cannot be saved",
                )
            } else if free < DISK_WARN_BYTES {
                Check::warn(ID, N, detail, "less than 1 GiB left; free some space")
            } else {
                Check::ok(ID, N, detail)
            }
        }
        Err(e) => Check::warn(
            ID,
            N,
            format!("cannot read free space: {e}"),
            "ignore if unsure",
        ),
    }
}

/// `local models`: an informational line with the detected hardware and the best-fitting curated
/// local model. Never a warning or failure: a machine without a GPU is not broken. Best effort,
/// local only.
#[must_use]
pub fn check_local_models(
    hw: &provider_catalog::HardwareInfo,
    advice: &provider_catalog::LocalAdvice,
) -> Check {
    const ID: &str = "local_models";
    const N: &str = "local models";
    let gib = |b: u64| format!("{} GiB", (b + (1 << 29)) >> 30);
    let ram = hw
        .ram_bytes
        .map_or_else(|| "RAM unknown".to_owned(), |b| format!("{} RAM", gib(b)));
    let gpu = if hw.unified_memory {
        "unified memory (Apple silicon)".to_owned()
    } else {
        match &hw.gpu {
            Some(g) => match g.vram_bytes {
                Some(v) => format!("{} ({} VRAM)", g.name, gib(v)),
                None => g.name.clone(),
            },
            None => "GPU unknown".to_owned(),
        }
    };
    let pick = match &advice.recommended {
        Some(id) => advice
            .models
            .iter()
            .find(|m| &m.model_id == id)
            .map_or_else(
                || "no recommendation".to_owned(),
                |m| format!("suggested: `{}`", m.pull_command),
            ),
        None => "no recommendation".to_owned(),
    };
    let detail = format!("{ram}, {gpu}; {pick} (best effort, nothing is installed for you)");
    if hw.ram_bytes.is_none() {
        Check::skipped(ID, N, detail)
    } else {
        Check::ok(ID, N, detail)
    }
}

/// Newest modification time of the files directly inside `dir`.
#[must_use]
pub fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

/// `clock`: the wall clock is plausible and did not jump back behind data
/// this installation already wrote.  Local only: it is not compared with a
/// network time source (no outbound request).
#[must_use]
pub fn check_clock(now: SystemTime, newest_data: Option<SystemTime>) -> Check {
    const ID: &str = "clock";
    const N: &str = "clock";
    let hint = "turn on automatic time sync in the operating system settings";
    let floor = SystemTime::UNIX_EPOCH + Duration::from_secs(CLOCK_FLOOR_SECS);
    if now < floor {
        return Check::fail(
            ID,
            N,
            "system clock is set before 2025; schedules and TLS will misbehave",
            hint,
        );
    }
    if let Some(newest) = newest_data
        && let Ok(ahead) = newest.duration_since(now)
        && ahead > CLOCK_SKEW_TOLERANCE
    {
        return Check::warn(
            ID,
            N,
            format!(
                "data files are {} min in the future: the clock went back",
                ahead.as_secs() / 60
            ),
            hint,
        );
    }
    Check::ok(
        ID,
        N,
        "plausible (checked locally, not against network time)",
    )
}

/// Clock difference from a network time source above which `clock_skew` warns.
pub const NETWORK_SKEW_TOLERANCE: Duration = Duration::from_secs(2 * 60);

/// Parse an HTTP `Date` header value (`Sun, 06 Nov 1994 08:49:37 GMT`).
#[must_use]
pub fn parse_http_date(value: &str) -> Option<SystemTime> {
    let t = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    u64::try_from(t.timestamp())
        .ok()
        .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))
}

/// `clock_skew`: this computer's time against the `Date` header of an HTTPS
/// endpoint the user chose (`remote` = that time, or why it could not be read).
/// An unreachable endpoint never fails the doctor: the check is skipped.
#[must_use]
pub fn check_clock_skew(local: SystemTime, remote: Result<SystemTime, String>) -> Check {
    const ID: &str = "clock_skew";
    const N: &str = "clock skew";
    let server = match remote {
        Ok(t) => t,
        Err(e) => return Check::ok(ID, N, format!("skipped: could not read network time ({e})")),
    };
    let (diff, ahead) = match local.duration_since(server) {
        Ok(d) => (d, true),
        Err(e) => (e.duration(), false),
    };
    if diff <= NETWORK_SKEW_TOLERANCE {
        return Check::ok(
            ID,
            N,
            format!("within {} s of network time", diff.as_secs()),
        );
    }
    Check::warn(
        ID,
        N,
        format!(
            "this computer's clock is about {} min {} network time; scheduled jobs and sign-ins can break",
            diff.as_secs() / 60,
            if ahead { "ahead of" } else { "behind" }
        ),
        "turn on automatic time sync and check the time zone in the operating system settings",
    )
}

/// One capability the daemon may lack, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CapabilityStatus {
    /// Capability id (`display`, `browser`, `desktop_automation`, ...).
    pub id: String,
    /// Whether it can be used now.
    pub available: bool,
    /// Why not, when unavailable.
    pub reason: Option<String>,
}

/// `capabilities`: one neutral line per unavailable capability, never an
/// error ("not available on this platform" is a fact, not a failure).
#[must_use]
pub fn check_capabilities(caps: &[CapabilityStatus]) -> Check {
    const ID: &str = "capabilities";
    const N: &str = "capabilities";
    let available: Vec<&str> = caps
        .iter()
        .filter(|c| c.available)
        .map(|c| c.id.as_str())
        .collect();
    let missing: Vec<String> = caps
        .iter()
        .filter(|c| !c.available)
        .map(|c| match &c.reason {
            Some(r) => format!("{} ({r})", c.id),
            None => c.id.clone(),
        })
        .collect();
    let mut detail = if available.is_empty() {
        "none available".to_owned()
    } else {
        format!("available: {}", available.join(", "))
    };
    if !missing.is_empty() {
        detail.push_str("; not available here: ");
        detail.push_str(&missing.join(", "));
    }
    if available.is_empty() && !caps.is_empty() {
        Check::skipped(ID, N, detail)
    } else {
        Check::ok(ID, N, detail)
    }
}

/// `desktop automation`: the OS automation backend (UI Automation on
/// Windows).  Unsupported platforms are neutral.
#[must_use]
pub fn check_desktop_automation(os: &str, caps: Option<&desktop_api::Capabilities>) -> Check {
    const ID: &str = "desktop_automation";
    const N: &str = "desktop automation";
    match caps {
        Some(c) if !c.has_display => Check::skipped(
            ID,
            N,
            format!("{} backend loaded, but no display is available", c.platform),
        ),
        Some(c) => {
            let mut parts = Vec::new();
            if c.accessibility_tree {
                parts.push("accessibility tree");
            }
            if c.screenshot {
                parts.push("screenshots");
            }
            if c.app_launch {
                parts.push("app launch");
            }
            Check::ok(ID, N, format!("{}: {}", c.platform, parts.join(", ")))
        }
        None if os == "windows" => Check::warn(
            ID,
            N,
            "the Windows automation backend is not running",
            "unset BOT_DESKTOP=off and restart the daemon; see the daemon log for the reason",
        ),
        None => Check::skipped(ID, N, "not available on this platform"),
    }
}

/// `browser` (discovery only, no launch): which browser the tools would
/// start, or why none is usable.
#[must_use]
pub fn check_browser_found(found: Result<&Path, &str>) -> Check {
    const ID: &str = "browser";
    const N: &str = "browser";
    match found {
        Ok(p) if browser_cdp::launch::is_snap_browser(p) => Check::fail(
            ID,
            N,
            "snap browser is unsupported under the hardened service",
            "install non-snap Chrome/Chromium or set BOT_BROWSER_PATH",
        ),
        Ok(p) => Check::ok(ID, N, format!("{} (found; not launched)", p.display())),
        Err(e) => Check::warn(
            ID,
            N,
            e.to_owned(),
            "install a non-snap Chrome/Chromium/Edge or set BOT_BROWSER_PATH",
        ),
    }
}

// ---------------------------------------------------------------- rendering

/// Render the checks as the `bot doctor` table.
///
/// # Errors
/// Write errors of `out`.
pub fn render_table(checks: &[Check], out: &mut dyn Write) -> std::io::Result<()> {
    let w = checks.iter().map(|c| c.title.len()).max().unwrap_or(0);
    for c in checks {
        writeln!(out, " {}  {:w$}  {}", c.status.symbol(), c.title, c.detail)?;
        if let (Some(h), Status::Warn | Status::Fail) = (&c.hint, c.status) {
            writeln!(out, "    {:w$}  fix: {h}", "")?;
        }
    }
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    let (fails, warns, skipped) = (
        count(Status::Fail),
        count(Status::Warn),
        count(Status::Skipped),
    );
    let ok = checks.len() - fails - warns - skipped;
    if skipped == 0 {
        writeln!(out, "\n{fails} failed, {warns} warning(s), {ok} ok")
    } else {
        writeln!(
            out,
            "\n{fails} failed, {warns} warning(s), {ok} ok, {skipped} skipped"
        )
    }
}

/// The checks as the `bot doctor --json` document.
#[must_use]
pub fn to_json(checks: &[Check]) -> Value {
    json!({
        "ok": checks.iter().all(|c| c.status != Status::Fail),
        "checks": checks.iter().map(|c| json!({
            "name": c.title,
            "status": c.status.word(),
            "detail": c.detail,
            "hint": c.hint,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------- redaction

/// The user's home directory as text (`USERPROFILE`, else `HOME`).
#[must_use]
pub fn home_dir() -> Option<String> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| h.len() > 3)
}

/// Length in bytes of the prefix of `hay` that equals `needle` ignoring
/// letter case (Unicode), if it does.
fn ci_prefix(hay: &str, needle: &str) -> Option<usize> {
    let mut h = hay.char_indices();
    for n in needle.chars() {
        let (_, c) = h.next()?;
        if c != n && !c.to_lowercase().eq(n.to_lowercase()) {
            return None;
        }
    }
    Some(h.next().map_or(hay.len(), |(i, _)| i))
}

/// Replace every case-insensitive occurrence of `needle` in `hay`.
fn replace_ci(hay: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return hay.to_owned();
    }
    let mut out = String::with_capacity(hay.len());
    let mut rest = hay;
    while let Some(c) = rest.chars().next() {
        if let Some(len) = ci_prefix(rest, needle) {
            out.push_str(with);
            rest = &rest[len..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// [`replace_ci`], but only where `needle` is not part of a longer word.
fn replace_ci_word(hay: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return hay.to_owned();
    }
    let mut out = String::with_capacity(hay.len());
    let mut rest = hay;
    let mut prev: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        if !prev.is_some_and(char::is_alphanumeric)
            && let Some(len) = ci_prefix(rest, needle)
            && !rest[len..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            out.push_str(with);
            prev = needle.chars().next_back();
            rest = &rest[len..];
            continue;
        }
        out.push(c);
        prev = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Whether `c` can be part of an e-mail local part or domain.
fn email_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '%')
}

/// Mask e-mail addresses (`[email]`).
#[must_use]
pub fn mask_emails(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = out.len();
            // Walk back over the local part already copied to `out`.
            for (idx, c) in out.char_indices().rev() {
                if email_char(c) {
                    start = idx;
                } else {
                    break;
                }
            }
            let mut end = i + 1;
            while end < chars.len() && email_char(chars[end]) {
                end += 1;
            }
            let domain: String = chars[i + 1..end].iter().collect();
            let domain = domain.trim_end_matches('.');
            if start < out.len() && domain.contains('.') && !domain.starts_with('.') {
                out.truncate(start);
                out.push_str("[email]");
                i += 1 + domain.chars().count();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Whether `c` belongs to a token-like run (keys, tokens, their URL-encoded,
/// base64 or percent-escaped forms).
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '%')
}

/// Whether `run` holds a percent-escape (`%2F`, `%3d`, ...).
fn has_percent_escape(run: &str) -> bool {
    run.as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

/// Mask token-like runs as `[redacted]`: 24+ characters of
/// `[A-Za-z0-9_+=%-]`, or 12+ of them when the run contains a percent-escape
/// (a URL-encoded value).
#[must_use]
pub fn mask_long_tokens(s: &str) -> String {
    mask_long_tokens_keeping(s, &KnownIds::default())
}

/// [`mask_long_tokens`], but runs that are (part of) an identifier in
/// `known` stay: model and provider ids from the catalog and the config.
#[must_use]
pub fn mask_long_tokens_keeping(s: &str, known: &KnownIds) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if known.tokens.contains(run.as_str()) {
            out.push_str(run);
        } else if run.len() >= 24 || (run.len() >= 12 && has_percent_escape(run)) {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in s.chars() {
        if token_char(ch) {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Words that mark the value after them as a credential.
const SECRET_WORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "auth",
    "bearer",
    "credential",
    "cookie",
    "session",
    "pwd",
];

/// Mask the value of `name = value`, `name: value` and `Bearer value` when the
/// name looks like a credential (`api_key`, `token`, `password`, ...).
/// Quoted values are masked up to their closing quote.
#[must_use]
pub fn mask_secret_assignments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut pos = 0;
    while pos < chars.len() {
        let ch = chars[pos];
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            word.push(ch);
            out.push(ch);
            pos += 1;
            continue;
        }
        let lower = word.to_lowercase();
        word.clear();
        let secretish = SECRET_WORDS.iter().any(|w| lower.contains(w));
        // `name = value` / `name: value` / `Bearer value`.
        let mut val = pos;
        while val < chars.len() && chars[val] == ' ' {
            val += 1;
        }
        let assign = val < chars.len() && matches!(chars[val], '=' | ':');
        let bearer = lower == "bearer" && ch == ' ';
        if !(secretish && (assign || bearer)) {
            out.push(ch);
            pos += 1;
            continue;
        }
        if assign {
            val += 1;
            while val < chars.len() && chars[val] == ' ' {
                val += 1;
            }
            // `Authorization: Bearer <value>`: the scheme stays, the value goes.
            for scheme in ["bearer ", "basic "] {
                let ahead: String = chars[val..].iter().take(scheme.len()).collect();
                if ahead.eq_ignore_ascii_case(scheme) {
                    val += scheme.len();
                    while val < chars.len() && chars[val] == ' ' {
                        val += 1;
                    }
                }
            }
        }
        // Copy up to the value, then skip the value.
        out.extend(&chars[pos..val]);
        let mut end = val;
        match chars.get(end) {
            Some(&quote @ ('"' | '\'')) => {
                end += 1;
                while end < chars.len() && chars[end] != quote && chars[end] != '\n' {
                    end += 1;
                }
                out.push(quote);
                out.push_str("[redacted]");
                if end < chars.len() && chars[end] == quote {
                    out.push(quote);
                    end += 1;
                }
            }
            Some(_) => {
                while end < chars.len()
                    && !chars[end].is_whitespace()
                    && !matches!(chars[end], ',' | ';' | ')' | ']' | '}' | '|' | '`')
                {
                    end += 1;
                }
                if end > val {
                    out.push_str("[redacted]");
                }
            }
            None => {}
        }
        pos = end;
    }
    out
}

fn loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[');
    h.eq_ignore_ascii_case("localhost")
        || h.starts_with("127.")
        || h.starts_with("::1")
        || h.starts_with("0.0.0.0")
}

/// Mask what a URL tells about its owner: credentials (`user:pass@`), the
/// host (unless loopback) and the whole query string.  The scheme and path
/// stay, so the report still says which kind of endpoint failed.
#[must_use]
pub fn mask_urls(s: &str) -> String {
    mask_urls_keeping(s, &KnownIds::default())
}

/// [`mask_urls`], but hosts of the built-in provider catalog stay
/// (`api.anthropic.com` identifies no one).
#[must_use]
pub fn mask_urls_keeping(s: &str, known: &KnownIds) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        let has_scheme = rest[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        out.push_str(&rest[..pos + 3]);
        let after = &rest[pos + 3..];
        let end = after
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | ')')
            })
            .unwrap_or(after.len());
        let url = &after[..end];
        rest = &after[end..];
        if !has_scheme {
            // `://` without a scheme: not a URL, copy as is.
            out.push_str(url);
            continue;
        }
        let auth_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let authority = &url[..auth_end];
        let (had_userinfo, host_port) = match authority.rsplit_once('@') {
            Some((_, h)) => (true, h),
            None => (false, authority),
        };
        let host = host_port
            .rsplit_once(':')
            .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
            .map_or(host_port, |(h, _)| h);
        if had_userinfo {
            out.push_str("[redacted]@");
        }
        if loopback_host(host) || known.hosts.contains(&host.to_ascii_lowercase()) {
            out.push_str(host_port);
        } else {
            out.push_str("[host]");
        }
        let tail = &url[auth_end..];
        match tail.find('?') {
            Some(q) => {
                out.push_str(&tail[..q]);
                out.push_str("?[redacted]");
            }
            None => out.push_str(tail),
        }
    }
    out.push_str(rest);
    out
}

/// Drop the source excerpt a parse error quotes (`3 | key = "..."` and the
/// `|  ^` marker lines): it is arbitrary config text.  The location line and
/// the message stay.
#[must_use]
pub fn strip_source_excerpts(s: &str) -> String {
    s.lines()
        .filter(|l| {
            let t = l
                .trim_start()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start();
            !t.starts_with('|')
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shortened peer keys (`abcdef12…`) as `[peer]`.
fn mask_short_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            run.push(ch);
            continue;
        }
        if ch == '\u{2026}' && run.len() == 8 {
            out.push_str("[peer]");
        } else {
            out.push_str(&run);
            out.push(ch);
        }
        run.clear();
    }
    out.push_str(&run);
    out
}

/// The identifying names of this machine and account (user name, host
/// name), from the environment, with their placeholders.  Names shorter than
/// 3 characters are left alone (too likely to hit ordinary words).
#[must_use]
pub fn machine_identities() -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = Vec::new();
    for (var, label) in [
        ("USERNAME", "[user]"),
        ("USER", "[user]"),
        ("LOGNAME", "[user]"),
        ("COMPUTERNAME", "[host]"),
        ("HOSTNAME", "[host]"),
    ] {
        if let Ok(value) = std::env::var(var)
            && value.chars().count() >= 3
            && !v.iter().any(|(x, _)| x.eq_ignore_ascii_case(&value))
        {
            v.push((value, label));
        }
    }
    v
}

/// Identifiers the daemon itself knows from trusted fields (catalog and
/// config): the token mask and the URL host mask keep them.  Real secrets
/// equal to one are still masked by the exact-value redactor, which runs
/// first.
#[derive(Debug, Default, Clone)]
pub struct KnownIds {
    /// Each identifier and each token run inside it.
    tokens: std::collections::HashSet<String>,
    /// Lower-case host names of the built-in catalog.
    hosts: std::collections::HashSet<String>,
}

impl KnownIds {
    /// Add an identifier (model id, provider id, ...).
    pub fn add_id(&mut self, id: &str) {
        self.tokens.insert(id.to_owned());
        for run in id.split(|c: char| !token_char(c)).filter(|r| !r.is_empty()) {
            self.tokens.insert(run.to_owned());
        }
    }

    /// Add the host of `url`.
    pub fn add_host_of(&mut self, url: &str) {
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host_port.rsplit_once(':').map_or(host_port, |(h, _)| h);
        if !host.is_empty() {
            self.hosts.insert(host.to_ascii_lowercase());
        }
    }

    /// Model and provider ids of the catalog and of `providers.toml` in
    /// `data_dir`, and the hosts of catalog providers that are not the
    /// user's own custom providers.
    #[must_use]
    pub fn collect(data_dir: &Path, catalog: Option<&Catalog>) -> Self {
        let mut k = Self::default();
        let cfg = ProviderConfig::load(&config_path(data_dir)).ok();
        let custom: std::collections::HashSet<&str> = cfg
            .iter()
            .flat_map(|c| c.providers.iter())
            .filter(|p| p.custom.is_some())
            .map(|p| p.id.as_str())
            .collect();
        for e in catalog.iter().flat_map(|c| c.all()) {
            k.add_id(&e.id);
            for m in &e.models {
                k.add_id(&m.id);
            }
            if !custom.contains(e.id.as_str()) {
                k.add_host_of(&e.base_url);
            }
        }
        for p in cfg.iter().flat_map(|c| c.providers.iter()) {
            k.add_id(&p.id);
            if let Some(m) = &p.default_model {
                k.add_id(m);
            }
            for m in p.model_caps.keys() {
                k.add_id(m);
            }
            for m in p.custom.iter().flat_map(|e| e.models.iter()) {
                k.add_id(&m.id);
            }
        }
        k
    }
}

/// How text is scrubbed before it leaves the machine (see [`redact_report`]).
#[derive(Default)]
pub struct Scrub<'a> {
    /// The daemon's secret redactor: known secret values and their encoded
    /// forms.
    pub secrets: Option<&'a dyn Fn(&str) -> String>,
    /// Home directory (becomes `~`).
    pub home: Option<&'a str>,
    /// User / host names and their placeholders (see [`machine_identities`]).
    pub identities: Vec<(String, &'static str)>,
    /// Identifiers from trusted fields that the pattern masks keep.
    pub known: KnownIds,
}

impl Scrub<'_> {
    /// Scrub one piece of text (see [`redact_report`]).
    #[must_use]
    pub fn text(&self, text: &str) -> String {
        // Known secrets first, on the untouched text, so no later rewrite can
        // split a literal value.
        let mut s = match self.secrets {
            Some(r) => r(text),
            None => text.to_owned(),
        };
        s = strip_source_excerpts(&s);
        if let Some(home) = self.home.filter(|h| h.len() > 3) {
            let home = home.trim_end_matches(['/', '\\']);
            s = replace_ci(&s, home, "~");
            s = replace_ci(&s, &home.replace('\\', "/"), "~");
            s = replace_ci(&s, &home.replace('/', "\\"), "~");
        }
        s = mask_urls_keeping(&s, &self.known);
        s = mask_secret_assignments(&s);
        s = mask_emails(&s);
        for (name, label) in &self.identities {
            s = replace_ci_word(&s, name, label);
        }
        s = mask_short_keys(&s);
        let s = mask_long_tokens_keeping(&s, &self.known);
        match self.secrets {
            Some(r) => r(&s),
            None => s,
        }
    }
}

/// Redact text that leaves the machine: known secrets (and their encoded
/// forms) are masked first, then the source excerpts of parse errors are
/// dropped, the home directory becomes `~`, and URL credentials, hosts and
/// queries, credential assignments (`api_key = ...`), e-mail addresses, the
/// user and host names, shortened peer keys and any remaining token-like run
/// are masked.
#[must_use]
pub fn redact_report(text: &str, scrub: &Scrub<'_>) -> String {
    scrub.text(text)
}

/// The last pass over a whole [`report_markdown`] text whose values were
/// already scrubbed: known secrets, e-mail addresses and token-like runs
/// only.  The Markdown structure (table rows start with `|`, like the source
/// excerpts [`strip_source_excerpts`] drops) and the fixed template stay.
#[must_use]
pub fn redact_report_text(markdown: &str, scrub: &Scrub<'_>) -> String {
    let s = match scrub.secrets {
        Some(r) => r(markdown),
        None => markdown.to_owned(),
    };
    let s = mask_long_tokens_keeping(&mask_emails(&s), &scrub.known);
    match scrub.secrets {
        Some(r) => r(&s),
        None => s,
    }
}

// ---------------------------------------------------------------- report

/// Config files the report may name (names only, never contents).
pub const CONFIG_FILES: &[&str] = &[
    "providers.toml",
    "host.toml",
    "mcp.toml",
    "web.toml",
    "email.toml",
    "channels.toml",
    "schedule.toml",
    "policy.json",
];

/// Which of [`CONFIG_FILES`] exist in `dir`.
#[must_use]
pub fn present_config_files(dir: &Path) -> Vec<String> {
    CONFIG_FILES
        .iter()
        .filter(|n| dir.join(n).is_file())
        .map(|n| (*n).to_owned())
        .collect()
}

/// What the support report says besides the checks.
#[derive(Debug, Clone)]
pub struct ReportInfo {
    /// Daemon / CLI version.
    pub version: String,
    /// HTTP API version.
    pub api_version: String,
    /// `os/arch`.
    pub platform: String,
    /// Data directory (redacted on output).
    pub data_dir: PathBuf,
    /// Config file names present in the data dir.
    pub config_files: Vec<String>,
}

/// A Markdown support report.  Each value is scrubbed with `scrub` before it
/// is escaped; run [`redact_report`] over the result too before it leaves the
/// process (the daemon route does).
#[must_use]
pub fn report_markdown(info: &ReportInfo, checks: &[Check], scrub: &Scrub<'_>) -> String {
    use std::fmt::Write as _;
    // Every value is scrubbed before Markdown escaping: escaping rewrites `|`
    // and newlines, which would defeat literal secret matching.
    let mut s = String::new();
    let _ = writeln!(s, "# Praxi diagnostics report\n");
    let _ = writeln!(s, "- Version: {}", info.version);
    let _ = writeln!(s, "- API: {}", info.api_version);
    let _ = writeln!(s, "- Platform: {}", info.platform);
    let _ = writeln!(
        s,
        "- Data dir: `{}`",
        scrub
            .text(&info.data_dir.display().to_string())
            .replace('`', "'")
    );
    let files = if info.config_files.is_empty() {
        "(none)".to_owned()
    } else {
        info.config_files.join(", ")
    };
    let _ = writeln!(s, "- Config files: {files}");
    let count = |st: Status| checks.iter().filter(|c| c.status == st).count();
    let _ = writeln!(
        s,
        "- Summary: {} failed, {} warning(s), {} ok, {} skipped\n",
        count(Status::Fail),
        count(Status::Warn),
        count(Status::Ok),
        count(Status::Skipped)
    );
    let _ = writeln!(s, "## Checks\n");
    let _ = writeln!(s, "| Status | Check | Detail |");
    let _ = writeln!(s, "| --- | --- | --- |");
    let cell = |t: &str| scrub.text(t).replace('|', "\\|").replace(['\n', '\r'], " ");
    for c in checks {
        let mut detail = cell(&c.detail);
        if let (Some(h), Status::Warn | Status::Fail) = (&c.hint, c.status) {
            let _ = write!(detail, " (fix: {})", cell(h));
        }
        let _ = writeln!(s, "| {} | {} | {detail} |", c.status.word(), cell(&c.title));
    }
    s
}

#[cfg(test)]
mod tests;
