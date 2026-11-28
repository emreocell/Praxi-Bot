//! Lifecycle of MCP servers: connect, reconnect with backoff, tool
//! registration, failure counting and quarantine.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::diagnostics::ExternalRedactor;
use parking_lot::Mutex;
use rmcp::{
    RoleClient,
    model::{
        CallToolRequestParams, CallToolResult, ClientRequest, JsonObject, PingRequest,
        ServerNotification, SubscriptionFilter,
    },
    service::{Peer, ServiceError},
};
use secret_store::SecretStore;
use tool_registry::{Tool, ToolRegistry};

use crate::pins::{MemoryPinStore, PinStore, ToolChange, change_of, new_tool_change, pin_of};
use crate::{
    client::{CallGuard, ElicitorSlot, Signals},
    config::{McpServerConfig, McpTransport, ToolFilter},
    diagnostics::StderrLog,
    discovery::discover,
    error::{McpError, McpErrorKind, Result},
    roots::{PathGuard, canonicalize_filesystem_args},
    session::Session,
    tool::{Aux, McpAuxAdapter, McpToolAdapter, qualified_name},
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

/// How long a server gets to answer the liveness probe sent after one of its
/// calls timed out.
const LIVENESS_PROBE: Duration = Duration::from_secs(5);
/// Quiet period after a `tools/list_changed` notification before the tools are
/// listed again, so a burst of notifications costs one discovery.
const LIST_CHANGED_DEBOUNCE: Duration = Duration::from_millis(250);

fn authorization_required(error: &rmcp::transport::DynamicTransportError) -> bool {
    use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error.error.as_ref());
    while let Some(current) = source {
        if current.is::<AuthRequiredError>() || current.is::<InsufficientScopeError>() {
            return true;
        }
        source = current.source();
    }
    false
}

/// Tunables of the bridge.
#[derive(Debug, Clone, Copy)]
pub struct McpPolicy {
    /// Consecutive failures (a failed connect cycle, a call that failed at
    /// transport level, or a call that timed out on a server that then did
    /// not answer a liveness probe either) after which a server is quarantined.
    /// Tool-level errors (`isError`), JSON-RPC errors answered by the server
    /// and a slow call on a server that still answers are *not* failures: the
    /// server is evidently alive.
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
    /// Tools disabled because the server changed their definition after the
    /// owner approved it; each carries the before/after of the description.
    pub changed_tools: Vec<ToolChange>,
    /// Most recent error message.
    pub last_error: Option<String>,
    /// Class of [`Self::last_error`] (`None` when there is no error).
    pub last_error_kind: Option<McpErrorKind>,
    /// The server asked for an OAuth sign-in (or its stored sign-in can no
    /// longer be used). Only the user can clear this, by signing in.
    pub auth_required: bool,
}

struct State {
    auth_required: bool,
    generation: u64,
    retired: bool,
    status: ServerStatus,
    failures: u32,
    tools: Vec<String>,
    /// Tools held back because their definition changed after approval.
    changed: Vec<ToolChange>,
    last_error: Option<String>,
    /// Meaningful only while `last_error` is set.
    last_error_kind: McpErrorKind,
}

/// Internal per-server state shared with the tool adapters.
pub(crate) struct Server {
    config: McpServerConfig,
    filter: ToolFilter,
    registry: ToolRegistry,
    policy: McpPolicy,
    secrets: Option<Arc<dyn SecretStore>>,
    redactor: Option<ExternalRedactor>,
    path_guard: Option<PathGuard>,
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
    /// Raised by every session of this server (list-changed notifications).
    signals: Arc<Signals>,
    pins: Arc<dyn PinStore>,
    /// Server id, sent when the set of held-back tools changed.
    change_events: tokio::sync::broadcast::Sender<String>,
    /// Told about OAuth token values before they are used.
    registrar: Option<crate::oauth::SecretRegistrar>,
    /// Whether the list-changed watcher task was started.
    watching: AtomicBool,
    /// A live HTTP request rejected its bearer, even if local expiry is later.
    refresh_required: AtomicBool,
    /// Orders OAuth credential writes against sign-out and removal.
    credentials: crate::oauth::CredentialGuard,
    /// When the bearer of the live session expires (Unix seconds): the
    /// session is replaced before then (review finding 13).
    bearer_expires_at: Mutex<Option<u64>>,
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

    /// Mark a tool call of run `run_id` in flight (elicitation correlation).
    pub(crate) fn begin_call(&self, run_id: &str) -> CallGuard {
        self.signals.begin_call(run_id)
    }

    fn oauth_options(
        &self,
        mut options: crate::oauth::SignInOptions,
    ) -> Result<crate::oauth::SignInOptions> {
        options.guard = Some(self.credentials.clone());
        if let Some(config) = &self.config.oauth {
            options.client_id.clone_from(&config.client_id);
            options
                .client_metadata_url
                .clone_from(&config.client_metadata_url);
            options.allow_loopback_http = config.allow_loopback_http;
            options.expected_issuer.clone_from(&config.issuer);
            options.scopes.clone_from(&config.scopes);
            options.offline_access = config.offline_access;
            options.client_secret = match &config.client_secret_ref {
                Some(name) => {
                    let value = self
                        .secrets
                        .as_ref()
                        .ok_or_else(|| {
                            McpError::Config("OAuth client secret requires a secret store".into())
                        })?
                        .get(name)
                        .map_err(|_| McpError::Store("cannot read OAuth client secret".into()))?
                        .ok_or_else(|| McpError::Config("OAuth client secret is missing".into()))?;
                    if let Some(register) = &self.registrar {
                        register(value.expose());
                    }
                    Some(value.expose().to_owned())
                }
                None => None,
            };
        }
        Ok(options)
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
    /// `error` as text through the daemon's production redactor, when one is
    /// installed: stored errors reach `GET /v1/mcp/servers` and stderr.
    fn redacted(&self, error: &McpError) -> String {
        let text = error.to_string();
        self.redactor.as_ref().map_or(text.clone(), |r| r(&text))
    }

    fn record_failure(&self, error: &McpError) {
        let mut st = self.state.lock();
        if st.retired {
            return;
        }
        st.failures = st.failures.saturating_add(1);
        st.last_error = Some(self.redacted(error));
        st.last_error_kind = error.kind();
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

    #[allow(clippy::too_many_lines)] // one sequential connect-and-classify flow
    async fn connect_session(&self, reset_on_success: bool) -> Result<Arc<Session>> {
        let mut slot = self.session.lock().await;
        self.check_usable()?;
        if let Some(s) = slot.as_ref() {
            // A live session keeps the bearer it was opened with: once that
            // enters the refresh window, reconnect (which refreshes) instead.
            let bearer_due = self.bearer_expires_at.lock().is_some_and(|at| {
                at.saturating_sub(crate::oauth::now_secs()) <= crate::oauth::REFRESH_WINDOW_SECS
            });
            if Self::is_alive(s) && !bearer_due {
                return Ok(Arc::clone(s));
            }
            if let Some(old) = slot.take() {
                old.request_close();
            }
        }
        self.check_usable()?;
        // A filesystem server is never started without an explicit, usable folder.
        let mut launch_config = self.config.clone();
        if let McpTransport::Stdio { args, .. } = &mut launch_config.transport
            && let Err(reason) = canonicalize_filesystem_args(args, self.path_guard.as_ref())
        {
            let e = McpError::NeedsFolder {
                server: self.config.id.clone(),
                reason,
            };
            self.record_failure(&e);
            return Err(e);
        }
        // Secrets are resolved fresh on every connect cycle and only held for
        // the attempts and the connection's diagnostic redactor. A missing secret means nothing is
        // spawned and nothing is retried.
        let mut secrets = match resolve_secrets(&self.config, self.secrets.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                self.record_failure(&e);
                return Err(e);
            }
        };
        // A stored OAuth sign-in for exactly this server and URL. It is looked
        // up (and refreshed when it is about to expire) on every connect cycle.
        if let (McpTransport::Http { url, .. }, Some(store)) =
            (&self.config.transport, self.secrets.as_ref())
        {
            // Boxed: the OAuth client's future is large and would be embedded in
            // every call future otherwise.
            match Box::pin(crate::oauth::stored_bearer(
                self.id(),
                url,
                store,
                self.registrar.as_ref(),
                self.refresh_required.swap(false, Ordering::AcqRel),
                &self.oauth_options(crate::oauth::SignInOptions::default())?,
            ))
            .await
            {
                Ok(Some(bearer)) => {
                    *self.bearer_expires_at.lock() = bearer.expires_at;
                    secrets = secrets.with_bearer(&bearer.token);
                }
                Ok(None) => *self.bearer_expires_at.lock() = None,
                Err(e) => {
                    self.note_connect_error(&e);
                    return Err(e);
                }
            }
        }
        let attempts = self.policy.backoff.max_attempts.max(1);
        let mut last = None;
        for attempt in 0..attempts {
            match open(
                &launch_config,
                &secrets,
                &self.stderr,
                self.policy.connect_timeout,
                self.redactor.clone(),
                Arc::clone(&self.signals),
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
                    st.auth_required = false;
                    if reset_on_success {
                        st.failures = 0;
                        st.last_error = None;
                    }
                    return Ok(session);
                }
                // Waiting for the user to sign in is not a failing server:
                // no retry, no failure count, no quarantine.
                Err(e @ McpError::AuthRequired(_)) => {
                    self.note_connect_error(&e);
                    return Err(e);
                }
                // A config error or a missing launcher cannot be fixed by retrying.
                Err(e @ (McpError::Config(_) | McpError::RuntimeMissing { .. })) => {
                    self.record_failure(&e);
                    return Err(e);
                }
                Err(e) => {
                    if matches!(self.config.transport, McpTransport::Http { .. }) {
                        // Offline HTTP endpoints fail once; callers may reconnect deliberately.
                        // Keep local launcher backoff, which also covers cold npx/uvx startup.
                        self.record_failure(&e);
                        return Err(e);
                    }
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

    /// Record why a connect did not happen. An authorization requirement is
    /// remembered as such; anything else is an ordinary failure.
    fn note_connect_error(&self, error: &McpError) {
        if matches!(error, McpError::AuthRequired(_)) {
            let mut st = self.state.lock();
            if !st.retired {
                st.auth_required = true;
                st.last_error = Some(self.redacted(error));
                if st.status == ServerStatus::Connected {
                    st.status = ServerStatus::Disconnected;
                }
            }
        } else {
            self.record_failure(error);
        }
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
        let mut params = CallToolRequestParams::new(remote_name.to_owned());
        params.arguments = arguments;
        self.request_inner(|peer| async move { peer.call_tool(params).await })
            .await
    }

    /// Run one request on the live session with the call timeout and the
    /// failure accounting of a tool call.
    pub(crate) async fn request<T, F, Fut>(&self, run: F) -> Result<T>
    where
        F: FnOnce(Peer<RoleClient>) -> Fut + Clone,
        Fut: std::future::Future<Output = std::result::Result<T, ServiceError>>,
    {
        self.until_retired(self.request_inner(run)).await
    }

    async fn request_inner<T, F, Fut>(&self, run: F) -> Result<T>
    where
        F: FnOnce(Peer<RoleClient>) -> Fut + Clone,
        Fut: std::future::Future<Output = std::result::Result<T, ServiceError>>,
    {
        let mut retried_auth = false;
        loop {
            self.check_usable()?;
            let session = self.ensure_connected(false).await?;
            let id = self.config.id.clone();
            // Dropping the request future (this timeout, or the caller's own
            // cancellation) makes rmcp send `notifications/cancelled` for it.
            let request = run.clone()(session.peer().clone());
            tokio::pin!(request);
            let outcome = await_call(
                self.policy.call_timeout,
                FORM_HARD_CAP,
                || self.signals.is_eliciting(),
                request.as_mut(),
            )
            .await;
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
                Ok(Err(ServiceError::TransportSend(ref e))) if authorization_required(e) => {
                    let error = McpError::AuthRequired(id);
                    let mut slot = self.session.lock().await;
                    // An old response must not drop a newer connection or force
                    // another refresh of credentials already rotated by it.
                    if slot
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &session))
                    {
                        if let Some(old) = slot.take() {
                            old.request_close();
                        }
                        if retried_auth {
                            self.note_connect_error(&error);
                        } else {
                            self.refresh_required.store(true, Ordering::Release);
                        }
                    }
                    drop(slot);
                    if retried_auth {
                        return Err(error);
                    }
                    retried_auth = true;
                    continue;
                }
                Ok(Err(e)) => McpError::Transport {
                    server: id,
                    message: e.to_string(),
                },
                Err(_) => {
                    let timeout = McpError::Timeout {
                        server: id,
                        seconds: self.policy.call_timeout.as_secs().max(1),
                    };
                    // One slow tool must not take the server down for the calls
                    // running next to it: the session is only dropped when the
                    // server does not answer anything any more.
                    if still_answers(&session).await {
                        return Err(session.redactor.error(timeout));
                    }
                    timeout
                }
            };
            let failure = session.redactor.error(failure);
            self.fail_session(&session, &failure).await;
            return Err(failure);
        }
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
        let before = held_back_keys(&st.changed);
        st.changed.clear();
        // Decided once: the first tool pinned must not make the rest of a
        // brand-new server's first listing look like "new tools".
        let first_listing = !self.pins.has_server(self.id());
        for tool in &listed {
            if !self.filter.permits(&tool.name) {
                continue;
            }
            // Trust on first use: the owner connected this server, so the
            // first definition is the approved one. A later difference keeps
            // the tool out of the registry (and so away from the model).
            let current = pin_of(tool);
            match self.pins.get(self.id(), &tool.name) {
                None if first_listing => self.pins.put(self.id(), &tool.name, current),
                None => {
                    tracing::warn!(
                        tool = %session.redactor.scrub(&tool.name),
                        "MCP tool was never approved; tool disabled"
                    );
                    st.changed.push(new_tool_change(&tool.name, current));
                    continue;
                }
                Some(pinned) if pinned.hash == current.hash => {}
                Some(pinned) => {
                    tracing::warn!(
                        tool = %session.redactor.scrub(&tool.name),
                        "MCP tool definition changed after approval; tool disabled"
                    );
                    st.changed.push(change_of(&tool.name, &pinned, current));
                    continue;
                }
            }
            let adapter: Arc<dyn Tool> = Arc::new(McpToolAdapter::new(Arc::clone(self), tool));
            let name = qualified_name(&self.config.id, &tool.name);
            if self.registry.register(adapter) {
                st.tools.push(name);
            } else {
                tracing::warn!(tool = %session.redactor.scrub(&name), "MCP tool name already registered; skipped");
            }
        }
        // Resources and prompts are reached through fixed bridge tools, only
        // for a server that declares the capability. `@` cannot appear in a
        // server tool name, and the user's tool filter applies to them too.
        let info = session.peer().peer_info();
        let capabilities = info.as_ref().map(|i| &i.capabilities);
        for aux in Aux::ALL {
            if !capabilities.is_some_and(|c| aux.supported_by(c))
                || !self.filter.permits(aux.remote_name())
            {
                continue;
            }
            let name = qualified_name(&self.config.id, aux.remote_name());
            let adapter: Arc<dyn Tool> = Arc::new(McpAuxAdapter::new(Arc::clone(self), aux));
            if self.registry.register(adapter) {
                st.tools.push(name);
            }
        }
        st.status = ServerStatus::Connected;
        st.failures = 0;
        st.last_error = None;
        let count = st.tools.len();
        let now = held_back_keys(&st.changed);
        drop(st);
        drop(slot);
        if now != before {
            // A card the owner may be reading is out of date: tell the host.
            let _ = self.change_events.send(self.id().to_owned());
        }
        self.watch_list_changes(&session);
        Ok(count)
    }

    /// Keep the registry in step with a server that changes its tools at
    /// runtime. One watcher task per registration re-lists (debounced) when a
    /// session signals a change; on a `2026-07-28` connection the notification
    /// only arrives on a `subscriptions/listen` stream, which is opened once
    /// per session.
    fn watch_list_changes(self: &Arc<Self>, session: &Arc<Session>) {
        if !self.watching.swap(true, Ordering::SeqCst) {
            let server = Arc::downgrade(self);
            let signals = Arc::clone(&self.signals);
            let mut retired = self.retired.subscribe();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = retired.wait_for(|gone| *gone) => return,
                        () = signals.tools_changed.notified() => {}
                    }
                    tokio::time::sleep(LIST_CHANGED_DEBOUNCE).await;
                    let Some(server) = server.upgrade() else {
                        return;
                    };
                    match server.sync_tools().await {
                        Ok(_) => {}
                        Err(McpError::UnknownServer(_)) => return,
                        Err(error) => {
                            tracing::warn!(server = %server.id(), %error, "MCP tool list refresh failed");
                        }
                    }
                }
            });
        }
        let Some(info) = session.peer().peer_info() else {
            return;
        };
        let filter = SubscriptionFilter::builder()
            .tools_list_changed()
            .build()
            .supported_by(&info.capabilities);
        if info.protocol_version.has_initialize()
            || filter.tools_list_changed != Some(true)
            || session.listening.swap(true, Ordering::SeqCst)
        {
            return;
        }
        // Only a peer handle: it does not keep the session alive, and the
        // stream (and this task) ends when the session closes.
        let peer = session.peer().clone();
        let signals = Arc::clone(&self.signals);
        tokio::spawn(async move {
            let Ok(mut stream) = peer.listen(filter).await else {
                return;
            };
            while let Ok(Some(notification)) = stream.next().await {
                if matches!(
                    notification,
                    ServerNotification::ToolListChangedNotification(_)
                ) {
                    signals.tools_changed.notify_one();
                }
            }
        });
    }

    fn retire(&self) {
        // Writers of the retired registration can no longer save credentials.
        self.credentials.revoke(|| ());
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
        // A server whose process died or whose connection dropped is only
        // noticed by the next call; do not report it as connected meanwhile.
        // (Same lock order as everywhere else: session, then state.)
        let dead = self
            .session
            .try_lock()
            .is_ok_and(|slot| slot.as_ref().is_some_and(|s| !Self::is_alive(s)));
        let st = self.state.lock();
        ServerSnapshot {
            id: self.config.id.clone(),
            status: if dead && st.status == ServerStatus::Connected {
                ServerStatus::Disconnected
            } else {
                st.status
            },
            consecutive_failures: st.failures,
            tools: st.tools.clone(),
            changed_tools: st.changed.clone(),
            last_error: st.last_error.clone(),
            last_error_kind: st.last_error.as_ref().map(|_| st.last_error_kind),
            auth_required: st.auth_required,
        }
    }
}

/// Whether the server still answers anything after one of its calls timed
/// out. A JSON-RPC error counts: `2026-07-28` servers have no `ping`.
async fn still_answers(session: &Session) -> bool {
    let probe = session
        .peer()
        .send_request(ClientRequest::PingRequest(PingRequest::default()));
    matches!(
        tokio::time::timeout(LIVENESS_PROBE, probe).await,
        Ok(Ok(_) | Err(ServiceError::McpError(_)))
    )
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

    /// Call one tool by its server-side name and return what it said as text
    /// (at most `max_bytes`, cut at a character boundary). For the owner's
    /// own "Test" button: the host picks a fixed read-only tool from the
    /// catalog and shows the result on the owner's screen only. It is not a
    /// bot call: no approval, no model, no tool filter.
    ///
    /// # Errors
    /// Not connected, the server refused, or the tool reported an error.
    pub async fn call_for_owner(
        &self,
        tool: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        max_bytes: usize,
    ) -> Result<String> {
        let result = self.0.call(tool, arguments).await?;
        let output = crate::tool::into_output(result).map_err(|e| McpError::Transport {
            server: self.0.id().to_owned(),
            message: e.to_string(),
        })?;
        let mut text = String::new();
        for part in &output.content {
            match part {
                tool_registry::ContentPart::Text { text: t } => text.push_str(t),
                tool_registry::ContentPart::Json { data } => text.push_str(&data.to_string()),
                tool_registry::ContentPart::Binary { .. } => continue,
            }
            text.push('\n');
        }
        if text.len() > max_bytes {
            let mut end = max_bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        Ok(text)
    }
}

/// `(tool, hash)` of every held-back tool, to tell whether the set changed.
fn held_back_keys(changed: &[ToolChange]) -> Vec<(String, String)> {
    changed
        .iter()
        .map(|c| (c.tool.clone(), c.hash.clone()))
        .collect()
}

/// Owns all configured MCP servers and keeps a [`ToolRegistry`] in sync with them.
pub struct McpManager {
    registry: ToolRegistry,
    policy: McpPolicy,
    secrets: Option<Arc<dyn SecretStore>>,
    redactor: Option<ExternalRedactor>,
    path_guard: Option<PathGuard>,
    registrar: Option<crate::oauth::SecretRegistrar>,
    pins: Arc<dyn PinStore>,
    change_events: tokio::sync::broadcast::Sender<String>,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
    elicitor: Arc<ElicitorSlot>,
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
            path_guard: None,
            registrar: None,
            pins: Arc::new(MemoryPinStore::default()),
            change_events: tokio::sync::broadcast::channel(32).0,
            servers: Mutex::new(BTreeMap::new()),
            elicitor: Arc::default(),
        }
    }

    /// Install the host's form for MCP elicitation requests. Until then (and
    /// for a request the host cannot place) every request is declined.
    pub fn set_elicitor(&self, elicitor: Arc<dyn tool_registry::Elicitor>) {
        *self.elicitor.write() = Some(elicitor);
    }

    /// Where approved tool fingerprints are kept (default: in memory, so a
    /// restart re-approves). A tool whose name, description or schema no longer
    /// matches its fingerprint is not registered until
    /// [`Self::approve_tool_changes`].
    #[must_use]
    pub fn with_pin_store(mut self, store: Arc<dyn PinStore>) -> Self {
        self.pins = store;
        self
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

    /// Host check applied to the folder of every filesystem server before it
    /// starts (protected locations are refused, see [`crate::root_problem`]).
    #[must_use]
    pub fn with_path_guard(mut self, guard: PathGuard) -> Self {
        self.path_guard = Some(guard);
        self
    }

    /// Registers secret values with the host's redactor (the daemon's
    /// `SharedRedactor::register`). OAuth access and refresh tokens are passed
    /// to it before they are stored or sent.
    #[must_use]
    pub fn with_secret_registrar(mut self, registrar: crate::oauth::SecretRegistrar) -> Self {
        self.registrar = Some(registrar);
        self
    }

    /// Begin an OAuth sign-in for the HTTP server `id`. **Call this only for
    /// an explicit user action**: it contacts the server's authorization
    /// server and opens a loopback listener. The caller sends the user's
    /// browser to [`crate::oauth::SignIn::authorization_url`], awaits
    /// [`crate::oauth::SignIn::finish`] and then calls [`Self::reenable`].
    ///
    /// # Errors
    /// Unknown server, not an HTTP server, no secret store, or the errors of
    /// [`crate::oauth::SignIn::start`].
    pub async fn start_sign_in(
        &self,
        id: &str,
        options: crate::oauth::SignInOptions,
    ) -> Result<crate::oauth::SignIn> {
        let server = self.get(id)?;
        let McpTransport::Http { url, .. } = &server.config.transport else {
            return Err(McpError::Config(format!(
                "server `{id}` is not an http server; OAuth sign-in does not apply"
            )));
        };
        let store = self.secrets.clone().ok_or_else(|| {
            McpError::Config(format!(
                "server `{id}`: OAuth sign-in needs a secret store to keep the tokens in"
            ))
        })?;
        let options = server.oauth_options(options)?;
        crate::oauth::SignIn::start(id, url, store, self.registrar.clone(), options).await
    }

    /// Forget the stored OAuth sign-in of server `id` and drop its session.
    /// Returns whether credentials existed.
    ///
    /// # Errors
    /// Unknown server, no secret store, or a secret-store failure.
    pub async fn sign_out(&self, id: &str) -> Result<bool> {
        let server = self.get(id)?;
        let store = self.secrets.clone().ok_or_else(|| {
            McpError::Config(format!("server `{id}`: no secret store is configured"))
        })?;
        // What the authorization server is asked to forget, read before the
        // local copy goes.
        let revoke = match &server.config.transport {
            McpTransport::Http { url, .. } => crate::oauth::token_to_revoke(id, url, &store)
                .await
                .map(|token| (url.clone(), token)),
            McpTransport::Stdio { .. } => None,
        };
        // Revoke first: a sign-in exchange or refresh in flight can no longer
        // save, so the delete below is final.
        let existed = server
            .credentials
            .revoke(|| {
                let client = store.delete(&crate::oauth::registered_client_name(id));
                let tokens = store.delete(&crate::oauth::credential_name(id));
                client.and(tokens)
            })
            .map_err(|_| McpError::Store("cannot delete OAuth credentials".to_owned()))?;
        server.drop_session().await;
        {
            let mut state = server.state.lock();
            state.generation = state.generation.wrapping_add(1);
            state.auth_required = false;
            state.last_error = None;
            if state.status == ServerStatus::Connected {
                state.status = ServerStatus::Disconnected;
            }
            for tool in state.tools.drain(..) {
                server.registry.unregister(&tool);
            }
        }
        // Then end the sign-in at the provider too. Best effort: the local
        // copy is already gone and a provider that is down must not keep the
        // owner from signing out.
        if let Some((url, token)) = revoke {
            let options = server
                .oauth_options(crate::oauth::SignInOptions::default())
                .unwrap_or_default();
            let _ = crate::oauth::revoke_at_issuer(id, &url, token, &options).await;
        }
        Ok(existed)
    }

    /// Whether a resource-bound authorization is stored, without network I/O.
    /// # Errors
    /// Unknown server, non-HTTP transport, or a secret-store failure.
    pub async fn has_stored_authorization(&self, id: &str) -> Result<bool> {
        use rmcp::transport::auth::CredentialStore as _;
        let server = self.get(id)?;
        let McpTransport::Http { url, .. } = &server.config.transport else {
            return Err(McpError::Config(
                "OAuth applies only to HTTP servers".into(),
            ));
        };
        let Some(store) = self.secrets.clone() else {
            return Ok(false);
        };
        let credentials =
            crate::oauth::SecretCredentialStore::new(store, id, url, self.registrar.clone())?
                .load()
                .await
                .map_err(|_| McpError::Store("cannot read OAuth state".into()))?;
        Ok(credentials.is_some_and(|c| c.token_response.is_some()))
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
            path_guard: self.path_guard.clone(),
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
                changed: Vec::new(),
                last_error: None,
                last_error_kind: McpErrorKind::Other,
                auth_required: false,
            }),
            operation: tokio::sync::Mutex::new(()),
            retired: tokio::sync::watch::channel(false).0,
            bots: Mutex::new(config.bots.clone()),
            signals: Arc::new(Signals::new(config.id.clone(), Arc::clone(&self.elicitor))),
            pins: Arc::clone(&self.pins),
            change_events: self.change_events.clone(),
            registrar: self.registrar.clone(),
            watching: AtomicBool::new(false),
            refresh_required: AtomicBool::new(false),
            credentials: crate::oauth::CredentialGuard::default(),
            bearer_expires_at: Mutex::new(None),
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
        self.pins.forget_server(id);
        Ok(())
    }

    /// Forget the approved tool fingerprints of `id`, so its next listing is
    /// approved afresh. Hosts call it when the owner removes a server or
    /// changes what it runs; [`Self::detach_server`] alone keeps them, so a
    /// settings edit cannot silently approve a changed tool.
    pub fn forget_tool_pins(&self, id: &str) {
        self.pins.forget_server(id);
    }

    /// Approve the definition of `tool` the owner was shown, identified by its
    /// `hash` (from [`ToolChange::hash`]), and register it. The server is
    /// re-listed first and the tool is pinned only if the definition offered
    /// *now* still has that hash.
    ///
    /// # Errors
    /// Unknown server, the reconnect/discovery failure, or
    /// [`McpError::ToolChangedAgain`] when the live definition differs from
    /// the one with `hash` (or the tool is no longer held back).
    pub async fn approve_tool_changes(&self, id: &str, tool: &str, hash: &str) -> Result<usize> {
        let server = self.get(id)?;
        server.sync_tools().await?;
        let live = server
            .state
            .lock()
            .changed
            .iter()
            .find(|c| c.tool == tool && c.hash == hash)
            .map(|c| c.pending.clone());
        let Some(pending) = live else {
            return Err(McpError::ToolChangedAgain {
                server: id.to_owned(),
                tool: tool.to_owned(),
            });
        };
        self.pins.put(id, tool, pending);
        server.sync_tools().await
    }

    /// Server ids whose set of held-back tools changed (a card shown to the
    /// owner may be stale).
    #[must_use]
    pub fn subscribe_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.change_events.subscribe()
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

/// The longest open forms may keep one call alive past `call_timeout`, however
/// the host bounds its own wait (`approval_timeout` 0 means "no limit" there).
/// A little over the host's own limit, so the host's `cancel` comes first.
const FORM_HARD_CAP: Duration = Duration::from_secs(16 * 60);

/// Wait for a call for `call_timeout`. While a form is open (`eliciting`) the
/// call is waiting for the owner, not for a stuck server, so it may run on, but
/// only until `hard_cap` has passed since the start: then the wait ends as the
/// timeout it is, and dropping the call cancels the open form.
async fn await_call<F: std::future::Future + Unpin>(
    call_timeout: Duration,
    hard_cap: Duration,
    eliciting: impl Fn() -> bool,
    mut request: F,
) -> std::result::Result<F::Output, tokio::time::error::Elapsed> {
    let started = tokio::time::Instant::now();
    let mut wait = call_timeout;
    loop {
        match tokio::time::timeout(wait, &mut request).await {
            Err(_) if eliciting() && started.elapsed() < hard_cap => {
                // Never spin on a tiny timeout, never overshoot the cap.
                wait = call_timeout.max(Duration::from_millis(50)).min(
                    hard_cap
                        .saturating_sub(started.elapsed())
                        .max(Duration::from_millis(1)),
                );
            }
            outcome => break outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_call_waiting_on_a_form_ends_at_the_hard_cap_even_when_the_form_never_closes() {
        let started = tokio::time::Instant::now();
        let outcome = await_call(
            Duration::from_secs(30),
            Duration::from_secs(600),
            || true,
            Box::pin(std::future::pending::<()>()),
        )
        .await;
        assert!(outcome.is_err());
        let waited = started.elapsed();
        assert!(waited >= Duration::from_secs(600), "{waited:?}");
        assert!(
            waited <= Duration::from_secs(600) + Duration::from_secs(1),
            "{waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_without_a_form_times_out_on_time_and_a_form_that_closes_stops_the_extension() {
        let started = tokio::time::Instant::now();
        let outcome = await_call(
            Duration::from_secs(30),
            Duration::from_secs(600),
            || false,
            Box::pin(std::future::pending::<()>()),
        )
        .await;
        assert!(outcome.is_err());
        assert_eq!(started.elapsed(), Duration::from_secs(30));

        // A form open during the first window only: the next window ends the call.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let outcome = await_call(
            Duration::from_secs(30),
            Duration::from_secs(600),
            || calls.fetch_add(1, Ordering::SeqCst) == 0,
            Box::pin(std::future::pending::<()>()),
        )
        .await;
        assert!(outcome.is_err());
        assert_eq!(started.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_that_finishes_while_a_form_is_open_returns_its_result() {
        let outcome = await_call(
            Duration::from_secs(30),
            Duration::from_secs(600),
            || true,
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(100)).await;
                7
            }),
        )
        .await;
        assert_eq!(outcome.unwrap(), 7);
    }

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
        let (client, server) = tokio::join!(
            crate::client::BridgeClient::default().serve(client_io),
            server.serve(server_io)
        );
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
