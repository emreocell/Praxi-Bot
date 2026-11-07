//! MCP servers of `bot serve`: loads `<data dir>/mcp.toml`, connects the
//! servers through `mcp-bridge`, registers their tools into the daemon's tool
//! registry, and implements the `/v1/mcp/*` management API
//! ([`daemon_api::McpControl`]).  Every change made through the API is saved
//! back to `mcp.toml` (write temp file, fsync, rename) *before* it is applied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use daemon_api::mcp::{
    McpCatalogEntryDto, McpCatalogInstallDto, McpRequiredSecretDto, McpServerChanged,
    McpServerDto, McpServerInput, McpStatus, McpToolDto, McpTransportInput, McpTransportKind,
    McpTransportObject,
};
use daemon_api::{McpControl, McpControlError};
use mcp_bridge::{
    CatalogEntry, McpConfigStore, McpError, McpManager, McpPolicy, McpServerConfig, McpTransport,
    ServerSnapshot, ServerStatus, TomlFileStore,
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
# args = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]\n\
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
    connecting: Mutex<HashSet<String>>,
    /// Last status announced per server (deduplicates events).
    announced: Mutex<HashMap<String, McpStatus>>,
    /// Serialises add / update / remove / reconnect (they save the file).
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
    ) -> Arc<Self> {
        Self::start_with_policy(data_dir, registry, secrets, McpPolicy::default())
    }

    /// [`Self::start`] with a custom bridge policy (tests use short timeouts).
    pub fn start_with_policy(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Option<Arc<dyn SecretStore>>,
        policy: McpPolicy,
    ) -> Arc<Self> {
        let path = data_dir.join(MCP_FILE);
        let store = TomlFileStore::new(&path);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, FILE_HEADER) {
                eprintln!("warning: cannot create {}: {e}", path.display());
            }
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
        let catalog = mcp_bridge::builtin_catalog().unwrap_or_else(|e| {
            eprintln!("warning: {e}");
            Vec::new()
        });
        let host = Arc::new(Self {
            manager,
            registry,
            store,
            catalog,
            configs: Mutex::new(loaded.iter().map(|c| (c.id.clone(), c.clone())).collect()),
            connecting: Mutex::new(
                loaded
                    .iter()
                    .filter(|c| c.enabled)
                    .map(|c| c.id.clone())
                    .collect(),
            ),
            announced: Mutex::new(HashMap::new()),
            ops: tokio::sync::Mutex::new(()),
            broken,
            events: broadcast::channel(64).0,
            poller: Mutex::new(None),
        });
        for c in &loaded {
            host.announced.lock().insert(c.id.clone(), host.status_of(&c.id));
        }
        // Connect in the background so a slow `npx` never delays the daemon.
        for c in loaded {
            let h = Arc::clone(&host);
            tokio::spawn(async move {
                let id = c.id.clone();
                if let Err(e) = h.manager.add_server(c).await {
                    eprintln!("warning: MCP server {id} not started: {e}");
                }
                h.connecting.lock().remove(&id);
                h.announce(&id);
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
        if self.connecting.lock().contains(name) {
            return McpStatus::Connecting;
        }
        match self.snapshot(name) {
            Some(s) => match s.status {
                ServerStatus::Connected => McpStatus::Connected,
                ServerStatus::Quarantined => McpStatus::Quarantined,
                ServerStatus::Disabled => McpStatus::Disconnected,
                ServerStatus::Disconnected if s.last_error.is_some() => McpStatus::Error,
                ServerStatus::Disconnected => McpStatus::Disconnected,
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
        let (transport, command, args, url) = match &cfg.transport {
            McpTransport::Stdio { command, args, .. } => (
                McpTransportKind::Stdio,
                Some(command.clone()),
                Some(args.clone()),
                None,
            ),
            McpTransport::Http { url, .. } => {
                (McpTransportKind::Http, None, None, Some(url.clone()))
            }
        };
        McpServerDto {
            name: cfg.id.clone(),
            transport,
            command,
            args,
            url,
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

    /// Connect `cfg` through the manager, tracking the `connecting` state.
    async fn connect_new(&self, cfg: McpServerConfig) {
        let id = cfg.id.clone();
        if cfg.enabled {
            self.connecting.lock().insert(id.clone());
            self.announce(&id);
        }
        if let Err(e) = self.manager.add_server(cfg).await {
            eprintln!("warning: MCP server {id} not started: {e}");
        }
        self.connecting.lock().remove(&id);
        self.announce(&id);
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
        bots: input.bots,
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
        let _op = self.ops.lock().await;
        self.check_writable()?;
        let name = input
            .name
            .clone()
            .ok_or_else(|| McpControlError::Invalid("`name` is required".into()))?;
        let cfg = to_config(&name, input)?;
        if self.configs.lock().contains_key(&name) {
            return Err(McpControlError::Conflict(format!(
                "mcp server {name} already exists"
            )));
        }
        self.persist(|m| {
            m.insert(name.clone(), cfg.clone());
        })?;
        self.connect_new(cfg.clone()).await;
        Ok(self.dto(&cfg))
    }

    async fn update(
        &self,
        name: &str,
        input: McpServerInput,
    ) -> Result<McpServerDto, McpControlError> {
        let _op = self.ops.lock().await;
        self.check_writable()?;
        self.config_of(name)?;
        if input.name.as_deref().is_some_and(|n| n != name) {
            return Err(McpControlError::Invalid(
                "the name in the body differs from the one in the path; delete and add to rename"
                    .into(),
            ));
        }
        let cfg = to_config(name, input)?;
        self.persist(|m| {
            m.insert(name.to_owned(), cfg.clone());
        })?;
        match self.manager.remove_server(name).await {
            Ok(()) | Err(McpError::UnknownServer(_)) => {}
            Err(e) => return Err(map_err(e)),
        }
        self.connect_new(cfg.clone()).await;
        Ok(self.dto(&cfg))
    }

    async fn remove(&self, name: &str) -> Result<(), McpControlError> {
        let _op = self.ops.lock().await;
        self.check_writable()?;
        self.config_of(name)?;
        self.persist(|m| {
            m.remove(name);
        })?;
        match self.manager.remove_server(name).await {
            Ok(()) | Err(McpError::UnknownServer(_)) => {}
            Err(e) => return Err(map_err(e)),
        }
        self.connecting.lock().remove(name);
        self.forget(name);
        Ok(())
    }

    async fn reconnect(&self, name: &str) -> Result<McpServerDto, McpControlError> {
        let _op = self.ops.lock().await;
        let cfg = self.config_of(name)?;
        if cfg.enabled {
            if self.snapshot(name).is_some() {
                self.connecting.lock().insert(name.to_owned());
                self.announce(name);
                // A failed connect shows as `status: error`, not as an HTTP error.
                if let Err(e) = self.manager.reenable(name).await {
                    eprintln!("warning: MCP server {name} reconnect failed: {e}");
                }
                self.connecting.lock().remove(name);
                self.announce(name);
            } else {
                // Never registered (it was invalid at startup): try as new.
                self.connect_new(cfg.clone()).await;
            }
        }
        Ok(self.dto(&cfg))
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
