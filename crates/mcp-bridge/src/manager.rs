//! Lifecycle of MCP servers: connect, reconnect with backoff, tool
//! registration, failure counting and quarantine.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, JsonObject},
    service::ServiceError,
};
use tool_registry::{Tool, ToolRegistry};

use crate::{
    config::{McpServerConfig, ToolFilter},
    error::{McpError, Result},
    tool::{McpToolAdapter, qualified_name},
    transport::{Session, StderrLog, open},
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
    /// Bound for spawn + MCP handshake.
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
    stderr: Arc<StderrLog>,
    /// The session; the async mutex also serialises (re)connects.
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    state: Mutex<State>,
}

impl Server {
    pub(crate) fn id(&self) -> &str {
        &self.config.id
    }

    fn is_alive(s: &Session) -> bool {
        !s.is_closed() && !s.peer().is_transport_closed()
    }

    fn check_usable(&self) -> Result<()> {
        match self.state.lock().status {
            ServerStatus::Quarantined => Err(McpError::Quarantined(self.config.id.clone())),
            ServerStatus::Disabled => Err(McpError::Disabled(self.config.id.clone())),
            ServerStatus::Disconnected | ServerStatus::Connected => Ok(()),
        }
    }

    /// Count a failure; quarantine (and hide the tools) when the threshold is hit.
    fn record_failure(&self, error: &McpError) {
        let mut st = self.state.lock();
        st.failures += 1;
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

    fn record_success(&self) {
        let mut st = self.state.lock();
        st.failures = 0;
        st.last_error = None;
    }

    /// Return a live session, (re)connecting with backoff if needed.
    /// `reset_on_success` is `true` for explicit connects and `false` for the
    /// lazy reconnect on the call path (otherwise a server that connects fine
    /// but crashes on every call would never reach the threshold).
    async fn ensure_connected(&self, reset_on_success: bool) -> Result<Arc<Session>> {
        let mut slot = self.session.lock().await;
        if let Some(s) = slot.as_ref() {
            if Self::is_alive(s) {
                return Ok(Arc::clone(s));
            }
            *slot = None;
        }
        self.check_usable()?;
        let attempts = self.policy.backoff.max_attempts.max(1);
        let mut last = None;
        for attempt in 0..attempts {
            match open(&self.config, &self.stderr, self.policy.connect_timeout).await {
                Ok(session) => {
                    let session = Arc::new(session);
                    *slot = Some(Arc::clone(&session));
                    {
                        let mut st = self.state.lock();
                        if st.status == ServerStatus::Disconnected {
                            st.status = ServerStatus::Connected;
                        }
                    }
                    if reset_on_success {
                        self.record_success();
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
        *self.session.lock().await = None;
    }

    /// Call a remote tool by its server-side name.
    pub(crate) async fn call(
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
                self.record_success();
                return Ok(result);
            }
            // The server answered with a JSON-RPC error: it is alive.
            Ok(Err(ServiceError::McpError(e))) => {
                self.record_success();
                return Err(McpError::Transport {
                    server: id,
                    message: e.to_string(),
                });
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
        self.drop_session().await;
        self.record_failure(&failure);
        Err(failure)
    }

    /// Connect, list the tools and (re)register the permitted ones.
    async fn sync_tools(self: &Arc<Self>) -> Result<usize> {
        let session = self.ensure_connected(true).await?;
        let listed = match session.peer().list_all_tools().await {
            Ok(t) => t,
            Err(e) => {
                let err = McpError::Transport {
                    server: self.config.id.clone(),
                    message: format!("tools/list: {e}"),
                };
                self.drop_session().await;
                self.record_failure(&err);
                return Err(err);
            }
        };
        let mut st = self.state.lock();
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
                tracing::warn!(tool = %name, "MCP tool name already registered; skipped");
            }
        }
        st.status = ServerStatus::Connected;
        st.failures = 0;
        st.last_error = None;
        Ok(st.tools.len())
    }

    fn unregister_all(&self) {
        let mut st = self.state.lock();
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

/// Owns all configured MCP servers and keeps a [`ToolRegistry`] in sync with them.
pub struct McpManager {
    registry: ToolRegistry,
    policy: McpPolicy,
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
            servers: Mutex::new(BTreeMap::new()),
        }
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
        config.validate()?;
        let server = Arc::new(Server {
            filter: config.tool_filter()?,
            registry: self.registry.clone(),
            policy: self.policy,
            stderr: Arc::default(),
            session: tokio::sync::Mutex::new(None),
            state: Mutex::new(State {
                status: if config.enabled {
                    ServerStatus::Disconnected
                } else {
                    ServerStatus::Disabled
                },
                failures: 0,
                tools: Vec::new(),
                last_error: None,
            }),
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
        if server.config.enabled {
            server.sync_tools().await
        } else {
            Ok(0)
        }
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
        let server = self.get(id)?;
        {
            let mut st = server.state.lock();
            if st.status == ServerStatus::Disabled {
                return Err(McpError::Disabled(id.to_owned()));
            }
            st.status = ServerStatus::Disconnected;
            st.failures = 0;
            st.last_error = None;
        }
        server.drop_session().await;
        server.sync_tools().await
    }

    /// Disconnect and forget a server, unregistering its tools.
    ///
    /// # Errors
    /// [`McpError::UnknownServer`].
    pub async fn remove_server(&self, id: &str) -> Result<()> {
        let server = self
            .servers
            .lock()
            .remove(id)
            .ok_or_else(|| McpError::UnknownServer(id.to_owned()))?;
        server.unregister_all();
        server.drop_session().await;
        Ok(())
    }

    /// Disconnect every server (tools stay registered but calls will reconnect lazily).
    pub async fn shutdown(&self) {
        let all: Vec<_> = self.servers.lock().values().cloned().collect();
        for s in all {
            s.drop_session().await;
        }
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

    /// The most recent stderr lines (at most 200) captured from a stdio server.
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
}
