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
    McpCatalogEntryDto, McpCatalogInstallDto, McpRequiredSecretDto, McpServerChanged, McpServerDto,
    McpServerInput, McpStatus, McpToolDto, McpTransportInput, McpTransportKind, McpTransportObject,
};
use daemon_api::{McpControl, McpControlError};
use mcp_bridge::{
    CatalogEntry, McpConfigStore, McpError, McpManager, McpPolicy, McpServerConfig,
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
# args = [\"-y\", \"@modelcontextprotocol/server-filesystem@2.2.0\", \".\"]\n\
# [server.secret_env]\n\
# API_KEY = \"mcp/files-key\"   # name of a secret, never its value\n";

/// The running MCP servers of a daemon.
pub struct McpHost {
    manager: McpManager,
    registry: ToolRegistry,
    store: TomlFileStore,
    catalog: Vec<CatalogEntry>,
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
    ) -> Arc<Self> {
        Self::start_with_policy(data_dir, registry, secrets, redactor, McpPolicy::default())
    }

    /// [`Self::start`] with a custom bridge policy (tests use short timeouts).
    pub fn start_with_policy(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Option<Arc<dyn SecretStore>>,
        redactor: Option<mcp_bridge::ExternalRedactor>,
        policy: McpPolicy,
    ) -> Arc<Self> {
        let path = data_dir.join(MCP_FILE);
        let store = TomlFileStore::new(&path);
        if !path.exists()
            && let Err(e) = std::fs::write(&path, FILE_HEADER)
        {
            eprintln!("warning: cannot create {}: {e}", path.display());
        }
        let (loaded, broken) = match store.load() {
            Ok(l) => (l, None),
            Err(e) => {
                eprintln!("warning: {e}; MCP servers are not started");
                (Vec::new(), Some(e.to_string()))
            }
        };
        let mut manager = McpManager::with_policy(registry.clone(), policy);
        if let Some(s) = secrets {
            manager = manager.with_secret_store(s);
        }
        // The daemon's live redactor masks every MCP diagnostic (stderr,
        // rmcp warnings, connect and discovery errors).
        if let Some(r) = redactor {
            manager = manager.with_redactor(r);
        }
        let catalog = mcp_bridge::builtin_catalog().unwrap_or_else(|e| {
            eprintln!("warning: {e}");
            Vec::new()
        });
        // Register synchronously before returning the host. Background futures
        // hold the exact registration, so removal/replacement cannot resurrect it.
        let registrations: Vec<_> = loaded
            .iter()
            .filter_map(|cfg| match manager.register_server(cfg.clone()) {
                Ok(handle) => Some((cfg.id.clone(), cfg.enabled, handle)),
                Err(error) => {
                    eprintln!("warning: MCP server {} not registered: {error}", cfg.id);
                    None
                }
            })
            .collect();
        let host = Arc::new(Self {
            manager,
            registry,
            store,
            catalog,
            configs: Mutex::new(loaded.iter().map(|c| (c.id.clone(), c.clone())).collect()),
            connecting: Mutex::new(HashMap::new()),
            next_connect: AtomicU64::new(1),
            announced: Mutex::new(HashMap::new()),
            ops: tokio::sync::Mutex::new(()),
            broken,
            events: broadcast::channel(64).0,
            poller: Mutex::new(None),
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
        let weak = Arc::downgrade(&host);
        *host.poller.lock() = Some(tokio::spawn(poll(weak)));
        host
    }

    /// Disconnect every server (stdio children exit) and stop the poller.
    pub async fn shutdown(&self) {
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
            tool_count: snap
                .as_ref()
                .map_or(0, |s| u32::try_from(s.tools.len()).unwrap_or(u32::MAX)),
            category: category_name(cfg.effective_category()),
            trust_hints: cfg.trust_hints,
            bots: cfg.bots.clone(),
        }
    }

    fn config_of(&self, name: &str) -> Result<McpServerConfig, McpControlError> {
        self.configs
            .lock()
            .get(name)
            .cloned()
            .ok_or_else(|| McpControlError::NotFound(format!("mcp server {name}")))
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
            eprintln!("warning: MCP server {id} not started: {error}");
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

fn category_name(c: PermissionCategory) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "accounts".to_owned())
}

fn map_err(e: McpError) -> McpControlError {
    match e {
        McpError::Config(m) => McpControlError::Invalid(m),
        McpError::UnknownServer(n) => McpControlError::NotFound(format!("mcp server {n}")),
        other => McpControlError::Internal(other.to_string()),
    }
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
    if let Some(v) = obj.get("trust_hints") {
        cfg.trust_hints = json_bool(v, "trust_hints")?;
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
        id: name.to_owned(),
        transport,
        enabled: input.enabled.unwrap_or(true),
        tool_allow: input.tool_allow,
        tool_exclude: input.tool_exclude,
        trust_hints: input.trust_hints,
        category,
        secret_headers: input.secret_headers,
        secret_env: input.secret_env,
        bots: input.bots.map(dedup_ids),
    };
    cfg.validate().map_err(map_err)?;
    Ok(cfg)
}

fn catalog_dto(e: &CatalogEntry) -> Option<McpCatalogEntryDto> {
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
        },
    })
}

#[async_trait]
impl McpControl for McpHost {
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
            let cfg = to_config(&name, input)?;
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
        let (cfg, old, handle, generation) = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            let prev = self.config_of(name)?;
            let cfg = merge_update(&prev, &input)?;
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
        let old = {
            let _op = self.ops.lock().await;
            self.check_writable()?;
            self.config_of(name)?;
            self.persist(|m| {
                m.remove(name);
            })?;
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
        self.catalog.iter().filter_map(catalog_dto).collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<McpServerChanged> {
        self.events.subscribe()
    }
}

impl Drop for McpHost {
    fn drop(&mut self) {
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
        McpHost::start_with_policy(dir, ToolRegistry::new(), None, None, fast_policy())
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
        assert_eq!(github.install.transport, McpTransportKind::Stdio);
        assert_eq!(
            github
                .install
                .secret_env
                .get("GITHUB_PERSONAL_ACCESS_TOKEN")
                .map(String::as_str),
            Some("mcp/github-token")
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
