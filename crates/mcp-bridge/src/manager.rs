//! Lifecycle of MCP servers: connect, reconnect with backoff, tool
//! registration, failure counting and quarantine.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crate::diagnostics::ExternalRedactor;
use parking_lot::Mutex;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, JsonObject},
    service::ServiceError,
};
use secret_store::SecretStore;
use tool_registry::{Tool, ToolRegistry};

use crate::{
    config::{McpServerConfig, ToolFilter},
    diagnostics::StderrLog,
    discovery::discover,
    error::{McpError, Result},
    session::Session,
    tool::{McpToolAdapter, qualified_name},
    transport::{open, resolve_secrets},
};

/// Exponential reconnect backoff: `initial`, `2*initial`, ... capped at `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Delay before the second attempt.
    pub initial: Duration,
    /// Upper bound of a single delay.
    pub max: Duration,
    /// Connection attempts per connect cycle (at least 1).
    pub max_attempts: u32,
}

impl Backoff {
    /// Delay to wait after the failed attempt number `attempt` (0-based).
    #[must_use]
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(10),
            max_attempts: 3,
        }
    }
}

/// Tunables of the bridge.
#[derive(Debug, Clone, Copy)]
pub struct McpPolicy {
    /// Consecutive failures (a failed connect cycle, or a call that failed at
    /// transport level or timed out) after which a server is quarantined.
    /// Tool-level errors (`isError`) and JSON-RPC errors answered by the
    /// server are *not* failures: the server is evidently alive.
    pub quarantine_after: u32,
    /// Reconnect backoff.
    pub backoff: Backoff,
    /// Bound for spawn + MCP handshake, and separately for the entire
    /// tools/list discovery (32 pages, 1024 tools, 4 MiB of schemas/metadata).
    pub connect_timeout: Duration,
    /// Bound for one tool call.
    pub call_timeout: Duration,
}

impl Default for McpPolicy {
    fn default() -> Self {
        Self {
            quarantine_after: 3,
            backoff: Backoff::default(),
            connect_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(120),
        }
    }
}

/// Coarse state of a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    /// `enabled = false` in the config.
    Disabled,
    /// Configured but not (or no longer) connected; reconnects lazily on call.
    Disconnected,
    /// Session is up.
    Connected,
    /// Too many consecutive failures; tools are hidden until [`McpManager::reenable`].
    Quarantined,
}

/// Snapshot of one server for UIs and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSnapshot {
    /// Server id.
    pub id: String,
    /// Current status.
    pub status: ServerStatus,
    /// Consecutive failures so far.
    pub consecutive_failures: u32,
    /// Names (`mcp:<id>/<tool>`) of the tools currently registered.
    pub tools: Vec<String>,
    /// Most recent error message.
    pub last_error: Option<String>,
}

struct State {
    generation: u64,
    retired: bool,
    status: ServerStatus,
    failures: u32,
    tools: Vec<String>,
    last_error: Option<String>,
}

/// Internal per-server state shared with the tool adapters.
pub(crate) struct Server {
    config: McpServerConfig,
    filter: ToolFilter,
    registry: ToolRegistry,
    policy: McpPolicy,
    secrets: Option<Arc<dyn SecretStore>>,
    redactor: Option<ExternalRedactor>,
    stderr: Arc<StderrLog>,
    /// The session; the async mutex also serialises (re)connects.
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    state: Mutex<State>,
    /// Discovery and explicit reconnects serialize only for this server.
    operation: tokio::sync::Mutex<()>,
    retired: tokio::sync::watch::Sender<bool>,
    /// Live copy of `config.bots`: adapters read this, so a `bots` change
    /// takes effect on the next call without restarting the server.
    bots: Mutex<Option<Vec<String>>>,
}

impl Server {
    /// The bots allowed right now (`None` = all, `Some([])` = none).
    pub(crate) fn bots(&self) -> Option<Vec<String>> {
        self.bots.lock().clone()
    }

    pub(crate) fn id(&self) -> &str {
        &self.config.id
    }

    pub(crate) fn config(&self) -> &McpServerConfig {
        &self.config
    }

    fn is_alive(s: &Session) -> bool {
        !s.is_closed() && !s.peer().is_transport_closed()
    }

    fn check_usable(&self) -> Result<()> {
        let st = self.state.lock();
        if st.retired {
            return Err(McpError::UnknownServer(self.id().to_owned()));
        }
        match st.status {
            ServerStatus::Quarantined => Err(McpError::Quarantined(self.config.id.clone())),
            ServerStatus::Disabled => Err(McpError::Disabled(self.config.id.clone())),
            ServerStatus::Disconnected | ServerStatus::Connected => Ok(()),
        }
    }

    async fn until_retired<T>(
        &self,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let mut retired = self.retired.subscribe();
        tokio::select! {
            biased;
            () = async {
                let already_retired = *retired.borrow_and_update();
                if !already_retired {
                    let _ = retired.changed().await;
                }
            } => Err(McpError::UnknownServer(self.id().to_owned())),
            result = work => result,
        }
    }

    /// Count a failure; quarantine (and hide the tools) when the threshold is hit.
    fn record_failure(&self, error: &McpError) {
        let mut st = self.state.lock();
        if st.retired {
            return;
        }
        st.failures = st.failures.saturating_add(1);
        st.last_error = Some(error.to_string());
        if st.status == ServerStatus::Connected {
            st.status = ServerStatus::Disconnected;
        }
        if st.failures >= self.policy.quarantine_after && st.status != ServerStatus::Quarantined {
            st.status = ServerStatus::Quarantined;
            for name in st.tools.drain(..) {
                self.registry.unregister(&name);
            }
            tracing::warn!(server = %self.config.id, failures = st.failures, "MCP server quarantined");
        }
    }

    async fn record_session_success(&self, session: &Arc<Session>) {
        let slot = self.session.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            let mut st = self.state.lock();
            if !st.retired {
                st.failures = 0;
                st.last_error = None;
            }
        }
    }

    /// An old in-flight call must not clear or quarantine a replacement session.
    async fn fail_session(&self, session: &Arc<Session>, error: &McpError) {
        let mut slot = self.session.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            if let Some(old) = slot.take() {
                old.request_close();
            }
            self.record_failure(error);
        }
    }

    /// Return a live session, (re)connecting with backoff if needed.
    /// `reset_on_success` is `true` for explicit connects and `false` for the
    /// lazy reconnect on the call path (otherwise a server that connects fine
    /// but crashes on every call would never reach the threshold).
    async fn ensure_connected(&self, reset_on_success: bool) -> Result<Arc<Session>> {
        self.until_retired(self.connect_session(reset_on_success))
            .await
    }

    async fn connect_session(&self, reset_on_success: bool) -> Result<Arc<Session>> {
        let mut slot = self.session.lock().await;
        self.check_usable()?;
        if let Some(s) = slot.as_ref() {
            if Self::is_alive(s) {
                return Ok(Arc::clone(s));
            }
            if let Some(old) = slot.take() {
                old.request_close();
            }
        }
        self.check_usable()?;
        // Secrets are resolved fresh on every connect cycle and only held for
        // the attempts and the connection's diagnostic redactor. A missing secret means nothing is
        // spawned and nothing is retried.
        let secrets = match resolve_secrets(&self.config, self.secrets.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                self.record_failure(&e);
                return Err(e);
            }
        };
        let attempts = self.policy.backoff.max_attempts.max(1);
        let mut last = None;
        for attempt in 0..attempts {
            match open(
                &self.config,
                &secrets,
                &self.stderr,
                self.policy.connect_timeout,
                self.redactor.clone(),
            )
            .await
            {
                Ok(session) => {
                    let session = Arc::new(session);
                    let mut st = self.state.lock();
                    if st.retired {
                        return Err(McpError::UnknownServer(self.id().to_owned()));
                    }
                    *slot = Some(Arc::clone(&session));
                    if st.status == ServerStatus::Disconnected {
                        st.status = ServerStatus::Connected;
                    }
                    if reset_on_success {
                        st.failures = 0;
                        st.last_error = None;
                    }
                    return Ok(session);
                }
                // A config error cannot be fixed by retrying.
                Err(e @ McpError::Config(_)) => {
                    self.record_failure(&e);
                    return Err(e);
                }
                Err(e) => {
                    tracing::warn!(server = %self.config.id, attempt, error = %e, "MCP connect failed");
                    last = Some(e);
                    if attempt + 1 < attempts {
                        tokio::time::sleep(self.policy.backoff.delay(attempt)).await;
                    }
                }
            }
        }
        let err = last.expect("at least one attempt was made");
        self.record_failure(&err);
        Err(err)
    }

    async fn drop_session(&self) {
        if let Some(old) = self.session.lock().await.take() {
            old.request_close();
        }
    }

    /// Cancel the session and wait until its stdio child (and, via the process
    /// group or job object, its wrapper tree) has exited or the timeout hits.
    async fn close_session(&self) {
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            close_session(session).await;
        }
    }

    /// Call a remote tool by its server-side name.
    pub(crate) async fn call(
        &self,
        remote_name: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult> {
        self.until_retired(self.call_inner(remote_name, arguments))
            .await
    }

    async fn call_inner(
        &self,
        remote_name: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult> {
        self.check_usable()?;
        let session = self.ensure_connected(false).await?;
        let mut params = CallToolRequestParams::new(remote_name.to_owned());
        params.arguments = arguments;
        let id = self.config.id.clone();
        let outcome =
            tokio::time::timeout(self.policy.call_timeout, session.peer().call_tool(params)).await;
        let failure = match outcome {
            Ok(Ok(result)) => {
                self.record_session_success(&session).await;
                return Ok(result);
            }
            // The server answered with a JSON-RPC error: it is alive.
            Ok(Err(ServiceError::McpError(e))) => {
                self.record_session_success(&session).await;
                return Err(session.redactor.error(McpError::Transport {
                    server: id,
                    message: e.to_string(),
                }));
            }
            Ok(Err(e)) => McpError::Transport {
                server: id,
                message: e.to_string(),
            },
            Err(_) => McpError::Timeout {
                server: id,
                seconds: self.policy.call_timeout.as_secs().max(1),
            },
        };
        let failure = session.redactor.error(failure);
        self.fail_session(&session, &failure).await;
        Err(failure)
    }

    /// Connect, list the tools and (re)register the permitted ones.
    async fn sync_tools(self: &Arc<Self>) -> Result<usize> {
        self.until_retired(async {
            let _operation = self.operation.lock().await;
            self.sync_tools_locked().await
        })
        .await
    }

    async fn reenable(self: &Arc<Self>) -> Result<usize> {
        self.until_retired(async {
            let _operation = self.operation.lock().await;
            {
                let mut st = self.state.lock();
                if st.retired {
                    return Err(McpError::UnknownServer(self.id().to_owned()));
                }
                if st.status == ServerStatus::Disabled {
                    return Err(McpError::Disabled(self.id().to_owned()));
                }
                st.status = ServerStatus::Disconnected;
                st.failures = 0;
                st.last_error = None;
            }
            self.drop_session().await;
            self.sync_tools_locked().await
        })
        .await
    }

    async fn sync_tools_locked(self: &Arc<Self>) -> Result<usize> {
        self.check_usable()?;
        let generation = {
            let mut st = self.state.lock();
            st.generation = st.generation.wrapping_add(1);
            st.generation
        };
        let session = self.ensure_connected(true).await?;
        let listed = match discover(&session, self.id(), self.policy.connect_timeout).await {
            Ok(tools) => tools,
            Err(error) => {
                self.fail_session(&session, &error).await;
                return Err(error);
            }
        };
        // Lock in the same order as connect/failure handling. A concurrent
        // call can replace the session without starting another discovery.
        let slot = self.session.lock().await;
        let mut st = self.state.lock();
        if st.retired || st.generation != generation {
            return Err(McpError::UnknownServer(self.id().to_owned()));
        }
        if !slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            return Err(McpError::Transport {
                server: self.id().to_owned(),
                message: "tools/list session was replaced before registration".to_owned(),
            });
        }
        if st.status == ServerStatus::Quarantined {
            return Err(McpError::Quarantined(self.id().to_owned()));
        }
        for name in st.tools.drain(..) {
            self.registry.unregister(&name);
        }
        for tool in &listed {
            if !self.filter.permits(&tool.name) {
                continue;
            }
            let adapter: Arc<dyn Tool> = Arc::new(McpToolAdapter::new(Arc::clone(self), tool));
            let name = qualified_name(&self.config.id, &tool.name);
            if self.registry.register(adapter) {
                st.tools.push(name);
            } else {
                tracing::warn!(tool = %session.redactor.scrub(&name), "MCP tool name already registered; skipped");
            }
        }
        st.status = ServerStatus::Connected;
        st.failures = 0;
        st.last_error = None;
        Ok(st.tools.len())
    }

    fn retire(&self) {
        let mut st = self.state.lock();
        st.retired = true;
        st.generation = st.generation.wrapping_add(1);
        st.status = ServerStatus::Disconnected;
        self.retired.send_replace(true);
        for name in st.tools.drain(..) {
            self.registry.unregister(&name);
        }
    }

    fn snapshot(&self) -> ServerSnapshot {
        let st = self.state.lock();
        ServerSnapshot {
            id: self.config.id.clone(),
            status: st.status,
            consecutive_failures: st.failures,
            tools: st.tools.clone(),
            last_error: st.last_error.clone(),
        }
    }
}

/// Signal the session's runtime owner and wait for its bounded cleanup.
/// In-flight callers may retain their peer handles without delaying shutdown.
async fn close_session(session: Arc<Session>) {
    session.close().await;
}

/// A particular registration, unaffected by removing/replacing the same id.
/// Hosts can prepare registry changes under a short configuration lock and do
/// remote work through this handle after releasing that lock.
#[derive(Clone)]
pub struct McpServerHandle(Arc<Server>);

impl McpServerHandle {
    /// Whether both handles are the same registration (not a re-registered copy).
    #[must_use]
    pub fn same_server(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Discover and register this server's tools, with the manager's limits.
    ///
    /// # Errors
    /// Connection/discovery failure, or this registration was removed.
    pub async fn connect(&self) -> Result<usize> {
        if !self.0.config.enabled {
            return Ok(0);
        }
        self.0.sync_tools().await
    }

    /// Clear quarantine and reconnect this registration.
    ///
    /// # Errors
    /// Disabled, removed, or connection/discovery failure.
    pub async fn reenable(&self) -> Result<usize> {
        self.0.reenable().await
    }

    /// Retire this registration and wait for its transport to close.
    pub async fn close(&self) {
        self.0.retire();
        self.0.close_session().await;
    }
}

/// Owns all configured MCP servers and keeps a [`ToolRegistry`] in sync with them.
pub struct McpManager {
    registry: ToolRegistry,
    policy: McpPolicy,
    secrets: Option<Arc<dyn SecretStore>>,
    redactor: Option<ExternalRedactor>,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
}

impl McpManager {
    /// Manager with the default [`McpPolicy`].
    #[must_use]
    pub fn new(registry: ToolRegistry) -> Self {
        Self::with_policy(registry, McpPolicy::default())
    }

    /// Manager with a custom policy.
    #[must_use]
    pub fn with_policy(registry: ToolRegistry, policy: McpPolicy) -> Self {
        Self {
            registry,
            policy,
            secrets: None,
            redactor: None,
            servers: Mutex::new(BTreeMap::new()),
        }
    }

    /// Secret store used to resolve `secret_headers` / `secret_env` at connect
    /// time.  Without one, servers that use secret references fail to start.
    #[must_use]
    pub fn with_secret_store(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(store);
        self
    }

    /// Live host redactor (the daemon's `SharedRedactor::redact`) applied to
    /// every MCP diagnostic, after the per-connection masking: server stderr,
    /// forwarded rmcp warnings and errors, and connect/discovery errors.
    #[must_use]
    pub fn with_redactor(mut self, redactor: ExternalRedactor) -> Self {
        self.redactor = Some(redactor);
        self
    }

    fn get(&self, id: &str) -> Result<Arc<Server>> {
        self.servers
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| McpError::UnknownServer(id.to_owned()))
    }

    /// Register a server.  If it is enabled it is connected immediately and its
    /// (filtered) tools are registered as `mcp:<id>/<tool>`; the number of
    /// registered tools is returned.  A server that fails to connect stays
    /// registered (counting one failure) so [`Self::connect`] can retry it.
    ///
    /// # Errors
    /// Invalid or duplicate config, or the connect failure.
    pub async fn add_server(&self, config: McpServerConfig) -> Result<usize> {
        self.register_server(config)?.connect().await
    }

    /// Register configuration without performing remote I/O. Connect the
    /// returned handle after releasing the host's configuration/persistence lock.
    ///
    /// # Errors
    /// Invalid or duplicate configuration.
    pub fn register_server(&self, config: McpServerConfig) -> Result<McpServerHandle> {
        config.validate()?;
        let server = Arc::new(Server {
            filter: config.tool_filter()?,
            registry: self.registry.clone(),
            policy: self.policy,
            secrets: self.secrets.clone(),
            redactor: self.redactor.clone(),
            stderr: Arc::default(),
            session: tokio::sync::Mutex::new(None),
            state: Mutex::new(State {
                generation: 0,
                retired: false,
                status: if config.enabled {
                    ServerStatus::Disconnected
                } else {
                    ServerStatus::Disabled
                },
                failures: 0,
                tools: Vec::new(),
                last_error: None,
            }),
            operation: tokio::sync::Mutex::new(()),
            retired: tokio::sync::watch::channel(false).0,
            bots: Mutex::new(config.bots.clone()),
            config,
        });
        {
            let mut servers = self.servers.lock();
            if servers.contains_key(server.id()) {
                return Err(McpError::Config(format!(
                    "server `{}` is already registered",
                    server.id()
                )));
            }
            servers.insert(server.id().to_owned(), Arc::clone(&server));
        }
        Ok(McpServerHandle(server))
    }

    /// The live `bots` list of server `id` (`None` = all bots).
    ///
    /// # Errors
    /// No server with this id is registered.
    pub fn server_bots(&self, id: &str) -> Result<Option<Vec<String>>> {
        Ok(self.get(id)?.bots())
    }

    /// Change who may use server `id` without touching its connection: calls
    /// already running finish, and the next call (and discovery) sees `bots`.
    ///
    /// # Errors
    /// No server with this id is registered.
    pub fn set_server_bots(&self, id: &str, bots: Option<Vec<String>>) -> Result<()> {
        *self.get(id)?.bots.lock() = bots;
        Ok(())
    }

    /// Capture a particular registration before releasing a host's lock.
    ///
    /// # Errors
    /// No server with this id is registered.
    pub fn server_handle(&self, id: &str) -> Result<McpServerHandle> {
        self.get(id).map(McpServerHandle)
    }

    /// Register many servers (e.g. from a config store), connecting them
    /// concurrently.  Returns, per server, the result of [`Self::add_server`].
    pub async fn add_servers(&self, configs: Vec<McpServerConfig>) -> Vec<(String, Result<usize>)> {
        let futures = configs.into_iter().map(|c| async move {
            let id = c.id.clone();
            (id, self.add_server(c).await)
        });
        futures::future::join_all(futures).await
    }

    /// (Re)connect a server and refresh its tool list.  Each failed cycle
    /// counts towards quarantine; a quarantined server must be re-enabled first.
    ///
    /// # Errors
    /// Unknown/quarantined/disabled server or the connect failure.
    pub async fn connect(&self, id: &str) -> Result<usize> {
        self.get(id)?.sync_tools().await
    }

    /// Manually lift a quarantine (also revives a failing server): clears the
    /// failure counter and reconnects.
    ///
    /// # Errors
    /// Unknown server, or the connect failure (which counts as a failure again).
    pub async fn reenable(&self, id: &str) -> Result<usize> {
        self.get(id)?.reenable().await
    }

    /// Disconnect and forget a server, unregistering its tools.
    ///
    /// # Errors
    /// [`McpError::UnknownServer`].
    pub async fn remove_server(&self, id: &str) -> Result<()> {
        self.detach_server(id)?.close().await;
        Ok(())
    }

    /// Remove and invalidate a registration without waiting for remote I/O.
    /// The returned handle can be closed outside a host's global lock.
    ///
    /// # Errors
    /// No server with this id is registered.
    pub fn detach_server(&self, id: &str) -> Result<McpServerHandle> {
        let mut servers = self.servers.lock();
        let server = servers
            .remove(id)
            .ok_or_else(|| McpError::UnknownServer(id.to_owned()))?;
        server.retire();
        Ok(McpServerHandle(server))
    }

    /// Cancel every session and wait for each stdio child to exit.
    pub async fn shutdown(&self) {
        let all: Vec<_> = self.servers.lock().values().cloned().collect();
        for server in &all {
            server.retire();
        }
        let closes = all.iter().map(|s| s.close_session());
        futures::future::join_all(closes).await;
    }

    /// Snapshot of one server.
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Option<ServerSnapshot> {
        self.servers.lock().get(id).map(|s| s.snapshot())
    }

    /// Snapshots of all servers, ordered by id.
    #[must_use]
    pub fn snapshots(&self) -> Vec<ServerSnapshot> {
        self.servers.lock().values().map(|s| s.snapshot()).collect()
    }

    /// Redacted stderr of a stdio server (at most 200 lines / 64 KiB).
    #[must_use]
    pub fn stderr_tail(&self, id: &str) -> Vec<String> {
        self.servers
            .lock()
            .get(id)
            .map(|s| s.stderr.snapshot())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let b = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(450),
            max_attempts: 5,
        };
        assert_eq!(b.delay(0), Duration::from_millis(100));
        assert_eq!(b.delay(1), Duration::from_millis(200));
        assert_eq!(b.delay(2), Duration::from_millis(400));
        assert_eq!(b.delay(3), Duration::from_millis(450));
        assert_eq!(b.delay(100), Duration::from_millis(450));
    }

    enum Reply {
        Never,
        Pages,
        Gated(&'static str),
        Immediate(&'static str),
    }

    struct DiscoveryServer {
        reply: Reply,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Arc<tokio::sync::Notify>,
    }

    impl rmcp::ServerHandler for DiscoveryServer {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            rmcp::model::ServerConfig::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
        }

        async fn list_tools(
            &self,
            request: Option<rmcp::model::PaginatedRequestParams>,
            _: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> std::result::Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            self.entered.send(()).unwrap();
            let name = match self.reply {
                Reply::Never => std::future::pending().await,
                Reply::Pages => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    "page"
                }
                Reply::Gated(name) => {
                    self.release.notified().await;
                    name
                }
                Reply::Immediate(name) => name,
            };
            let mut result =
                rmcp::model::ListToolsResult::with_all_items(vec![rmcp::model::Tool::new(
                    name,
                    "fixture",
                    Arc::default(),
                )]);
            if matches!(self.reply, Reply::Pages) {
                let cursor = request.and_then(|r| r.cursor).unwrap_or_default();
                result.next_cursor = Some(format!("{cursor}x"));
            }
            Ok(result)
        }
    }

    /// Fully initialized MCP session over memory pipes: no subprocesses,
    /// sockets, environment mutation, or wall-clock sleeps.
    async fn connected(
        manager: &McpManager,
        id: &str,
        reply: Reply,
    ) -> (
        McpServerHandle,
        tokio::sync::mpsc::UnboundedReceiver<()>,
        Arc<tokio::sync::Notify>,
        rmcp::service::RunningService<rmcp::RoleServer, DiscoveryServer>,
    ) {
        use rmcp::ServiceExt;
        let handle = manager
            .register_server(McpServerConfig::stdio(id, "unused-in-memory", [""; 0]))
            .unwrap();
        let (entered, received) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let server = DiscoveryServer {
            reply,
            entered,
            release: Arc::clone(&release),
        };
        let (client_io, server_io) = tokio::io::duplex(16 * 1024);
        let (client, server) = tokio::join!(().serve(client_io), server.serve(server_io));
        *handle.0.session.lock().await = Some(Arc::new(Session::from_running(
            client.unwrap(),
            crate::diagnostics::Redactor::default(),
        )));
        handle.0.state.lock().status = ServerStatus::Connected;
        (handle, received, release, server.unwrap())
    }

    fn short_deadline() -> McpPolicy {
        McpPolicy {
            connect_timeout: Duration::from_secs(3),
            ..McpPolicy::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn initialized_server_that_never_lists_tools_times_out() {
        let registry = ToolRegistry::new();
        let manager = McpManager::with_policy(registry.clone(), short_deadline());
        let (handle, mut entered, _, _server) = connected(&manager, "hung", Reply::Never).await;
        let discovery = tokio::spawn(async move { handle.connect().await });
        entered.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(matches!(
            discovery.await.unwrap(),
            Err(McpError::Timeout { .. })
        ));
        let snapshot = manager.snapshots().pop().unwrap();
        assert_eq!(snapshot.status, ServerStatus::Disconnected);
        assert_eq!(snapshot.consecutive_failures, 1);
        assert!(registry.list().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn discovery_deadline_covers_all_pages_together() {
        let registry = ToolRegistry::new();
        let manager = McpManager::with_policy(registry.clone(), short_deadline());
        let (handle, mut entered, _, _server) = connected(&manager, "pages", Reply::Pages).await;
        let discovery = tokio::spawn(async move { handle.connect().await });
        entered.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        entered.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            discovery.await.unwrap(),
            Err(McpError::Timeout { .. })
        ));
        assert!(
            registry.list().is_empty(),
            "partial lists must never be published"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn removed_discovery_cannot_publish_or_remove_replacement_tools() {
        let registry = ToolRegistry::new();
        let manager = McpManager::with_policy(registry.clone(), short_deadline());
        let (old, mut entered, release, _old_server) =
            connected(&manager, "same", Reply::Gated("obsolete")).await;
        let old_connect = old.clone();
        let discovery = tokio::spawn(async move { old_connect.connect().await });
        entered.recv().await.unwrap();
        let retired = manager.detach_server("same").unwrap();
        let (new, _entered, _, _new_server) =
            connected(&manager, "same", Reply::Immediate("current")).await;
        assert_eq!(new.connect().await.unwrap(), 1);
        release.notify_one();
        assert!(matches!(
            discovery.await.unwrap(),
            Err(McpError::UnknownServer(_))
        ));
        assert!(matches!(
            old.connect().await,
            Err(McpError::UnknownServer(_))
        ));
        retired.close().await;
        assert!(registry.get("mcp:same/obsolete").is_none());
        assert!(registry.get("mcp:same/current").is_some());
        assert_eq!(manager.snapshots()[0].tools, ["mcp:same/current"]);
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn stale_session_completion_cannot_publish_or_fail_a_reconnected_session() {
        let registry = ToolRegistry::new();
        let manager = McpManager::with_policy(registry.clone(), short_deadline());
        let (handle, mut entered, release, _old_server) =
            connected(&manager, "replaced", Reply::Gated("obsolete")).await;
        let old = handle.0.session.lock().await.as_ref().unwrap().clone();
        let pending = handle.clone();
        let discovery = tokio::spawn(async move { pending.connect().await });
        entered.recv().await.unwrap();

        // Deliberately leave the old transport alive to model a response
        // already queued when a failed connection was replaced.
        let first_failure = McpError::Transport {
            server: "replaced".to_owned(),
            message: "first failure".to_owned(),
        };
        handle.0.session.lock().await.take();
        handle.0.record_failure(&first_failure);
        let (donor, _new_entered, _, _new_server) =
            connected(&manager, "donor", Reply::Immediate("current")).await;
        let new = donor.0.session.lock().await.take().unwrap();
        *handle.0.session.lock().await = Some(Arc::clone(&new));
        handle.0.state.lock().status = ServerStatus::Connected;

        // Neither a late success nor another failure on the old connection
        // may change the new session's consecutive-failure state.
        handle.0.record_session_success(&old).await;
        handle.0.fail_session(&old, &first_failure).await;
        assert_eq!(
            manager.snapshot("replaced").unwrap().consecutive_failures,
            1
        );
        release.notify_one();
        assert!(
            discovery
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("replaced")
        );
        assert!(registry.list().is_empty());
        assert!(Arc::ptr_eq(
            handle.0.session.lock().await.as_ref().unwrap(),
            &new
        ));
        assert!(!new.is_closed());
        assert_eq!(handle.connect().await.unwrap(), 1);
        assert!(registry.get("mcp:replaced/current").is_some());
        assert!(registry.get("mcp:replaced/obsolete").is_none());
        drop((old, new));
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn failing_a_session_closes_it_even_while_a_caller_holds_an_arc() {
        let manager = McpManager::new(ToolRegistry::new());
        let (handle, _entered, _, _server) =
            connected(&manager, "failed", Reply::Immediate("tool")).await;
        let caller = handle.0.session.lock().await.as_ref().unwrap().clone();
        assert!(!caller.is_closed());
        handle
            .0
            .fail_session(
                &caller,
                &McpError::Transport {
                    server: "failed".into(),
                    message: "call failed".into(),
                },
            )
            .await;
        assert!(caller.is_closed());
        assert!(handle.0.session.lock().await.is_none());
        caller.close().await;
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn discovery_operations_serialize_for_one_registration() {
        let manager = McpManager::new(ToolRegistry::new());
        let (handle, mut entered, release, _server) =
            connected(&manager, "serial", Reply::Gated("tool")).await;
        let first_handle = handle.clone();
        let first = tokio::spawn(async move { first_handle.connect().await });
        entered.recv().await.unwrap();
        let second = tokio::spawn(async move { handle.connect().await });
        tokio::task::yield_now().await;
        assert!(
            entered.try_recv().is_err(),
            "second request must wait for the server operation lock"
        );
        release.notify_one();
        assert_eq!(first.await.unwrap().unwrap(), 1);
        entered.recv().await.unwrap();
        release.notify_one();
        assert_eq!(second.await.unwrap().unwrap(), 1);
        manager.shutdown().await;
    }
}
