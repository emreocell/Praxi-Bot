//! `bot serve`: assemble storage, providers, tools and the agent loop, then
//! run the daemon API (`daemon-api`) until Ctrl-C / SIGTERM / console close.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers, SharedRedactor};
use builtin_tools::{BuiltinConfig, ResolverGate};
use daemon_api::{
    AppState, DaemonConfig, McpControl, ProviderHost, load_or_create_token, token_path,
};
use desktop_api::DesktopBackend;
use permission_engine::{PermissionResolver, Policy};
use protocol::InitiatorKind;
use provider_catalog::{Auth, Catalog, ProviderConfig, ProviderEntry, config_path};
use secret_store::{PASSPHRASE_ENV, SecretStore};
use serde::Deserialize;
use skills::{SkillIndex, SkillRoots};
use storage::Storage;
use tool_registry::ToolRegistry;
use workflow_replay::{Handles, WorkflowStore};

use crate::Result;
use crate::channels;

/// Providers from `providers.toml`: every enabled one that can be built
/// (including runnable `cli:*` engines), the effective default active.
///
/// # Errors
/// Provider configuration cannot be read or parsed.
pub fn build_providers(
    data_dir: &Path,
    catalog: &Catalog,
    store: &dyn SecretStore,
    warn: &mut dyn FnMut(&str),
) -> Result<Providers> {
    let mut wrapped = |msg: &str| {
        if msg.starts_with("setup required") {
            warn(
                "setup required: no LLM provider is configured yet; chats will fail until you run `bot setup` or `bot providers add <id>`",
            );
        } else {
            warn(msg);
        }
    };
    Ok(agent_core::build_providers(
        data_dir,
        catalog,
        store,
        &mut wrapped,
    )?)
}

/// How often the daemon re-reads the secret store into the redactor, so a
/// secret stored after startup is masked too.
pub const REDACTOR_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Environment variable name suffixes whose values are credentials.
const ENV_SECRET_SUFFIXES: [&str; 3] = ["_API_KEY", "_TOKEN", "_SECRET"];

/// Values of credential-looking environment variables (`*_API_KEY`, `*_TOKEN`,
/// `*_SECRET`) and of the provider key variables named by the catalog or
/// `providers.toml` (`catalog_vars`); values shorter than 6 bytes are skipped.
fn env_secret_values(
    vars: impl IntoIterator<Item = (String, String)>,
    catalog_vars: &[String],
) -> Vec<String> {
    vars.into_iter()
        .filter(|(name, value)| {
            value.len() >= 6
                && (catalog_vars.iter().any(|c| c == name)
                    || ENV_SECRET_SUFFIXES
                        .iter()
                        .any(|s| name.to_ascii_uppercase().ends_with(s)))
        })
        .map(|(_, value)| value)
        .collect()
}

const BUILTIN_CATALOG_SOURCES: &[&str] = &[
    include_str!("../../../providers/catalog/anthropic.toml"),
    include_str!("../../../providers/catalog/deepseek.toml"),
    include_str!("../../../providers/catalog/gemini-openai-compat.toml"),
    include_str!("../../../providers/catalog/gemini.toml"),
    include_str!("../../../providers/catalog/groq.toml"),
    include_str!("../../../providers/catalog/lmstudio.toml"),
    include_str!("../../../providers/catalog/mistral.toml"),
    include_str!("../../../providers/catalog/ollama.toml"),
    include_str!("../../../providers/catalog/openrouter.toml"),
    include_str!("../../../providers/catalog/together.toml"),
];

/// Names of the environment variables that hold provider API keys: every
/// `ApiKeyEnv` of the built-in catalog and of the user's custom providers.
fn catalog_env_vars(data_dir: &Path) -> Vec<String> {
    let mut entries: Vec<ProviderEntry> = BUILTIN_CATALOG_SOURCES
        .iter()
        .filter_map(|src| toml::from_str(src).ok())
        .collect();
    if let Ok(cfg) = ProviderConfig::load(&config_path(data_dir)) {
        entries.extend(cfg.providers.into_iter().filter_map(|p| p.custom));
    }
    entries
        .into_iter()
        .filter_map(|e| match e.auth {
            Auth::ApiKeyEnv(var) => Some(var),
            Auth::None => None,
        })
        .collect()
}

/// The vault passphrase from the environment, if any.
fn env_passphrase() -> Option<String> {
    std::env::var(PASSPHRASE_ENV).ok().filter(|p| !p.is_empty())
}

/// Where the redactor's secrets come from; re-read on every refresh.
#[derive(Clone)]
struct RedactorSources {
    store: Option<Arc<dyn SecretStore>>,
    api_token: String,
    passphrase: Option<String>,
    /// Provider key environment variables (see [`catalog_env_vars`]).
    catalog_vars: Vec<String>,
}

impl RedactorSources {
    /// Literal secrets outside the secret store: the daemon bearer token, the
    /// vault passphrase and credential environment variables.
    fn extras(&self) -> Vec<String> {
        let mut v = vec![self.api_token.clone()];
        v.extend(self.passphrase.iter().filter(|p| !p.is_empty()).cloned());
        v.extend(env_secret_values(std::env::vars(), &self.catalog_vars));
        v
    }

    /// Re-read everything into `handle`; on failure the previous contents stay.
    fn refresh(&self, handle: &SharedRedactor) {
        if let Err(e) = handle.refresh_from_store(self.store.as_deref(), &self.extras()) {
            eprintln!("warning: secret redaction refresh failed (keeping the previous set): {e}");
        }
    }
}

/// The production redactor: the built-in key shapes, every value in the secret
/// store, the daemon bearer token, the vault passphrase and credential
/// environment variables, each also in JSON/URL/base64 form.  A store that
/// cannot be read still yields the shapes plus the other secrets (fail closed).
/// Refresh it with [`SharedRedactor::refresh_now`] (installed here).
fn production_redactor(sources: &RedactorSources) -> SharedRedactor {
    let handle = SharedRedactor::default();
    let extras = sources.extras();
    if let Err(e) = handle.refresh_from_store(sources.store.as_deref(), &extras) {
        eprintln!("warning: secret redaction: cannot read the secret store: {e}");
        let _ = handle.refresh_from_store(None, &extras);
    }
    let s = sources.clone();
    handle.set_refresher(Arc::new(move |h| s.refresh(h)));
    handle
}

/// The production redactor's secret list, read once, for a report written
/// outside the daemon (`bot doctor --report`): every value in `store`, the
/// API token in `data_dir` (as stored, even if malformed), the vault
/// passphrase and credential environment variables, each with its encoded
/// variants.  Unlike [`production_redactor`] it fails closed: a store that
/// cannot be read is an error, never a smaller secret list.
///
/// # Errors
/// The secret store cannot be listed or read.
pub fn report_redactor(
    data_dir: &Path,
    store: &dyn SecretStore,
) -> std::result::Result<SharedRedactor, secret_store::SecretError> {
    let sources = RedactorSources {
        store: None,
        api_token: std::fs::read_to_string(daemon_api::token_path(data_dir))
            .map(|t| t.trim().to_owned())
            .unwrap_or_default(),
        passphrase: env_passphrase(),
        catalog_vars: catalog_env_vars(data_dir),
    };
    let extras: Vec<String> = sources
        .extras()
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect();
    let handle = SharedRedactor::default();
    handle.refresh_from_store(Some(store), &extras)?;
    Ok(handle)
}

/// A secret store that masks every value written through it from the moment
/// it is stored (the daemon's write-only secrets route, MCP, channels), not
/// only after the next refresh.
struct RegisteringStore {
    inner: Arc<dyn SecretStore>,
    redactor: SharedRedactor,
}

impl SecretStore for RegisteringStore {
    fn get(
        &self,
        name: &str,
    ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> std::result::Result<(), secret_store::SecretError> {
        self.inner.set(name, value)?;
        self.redactor.register(value);
        Ok(())
    }
    fn delete(&self, name: &str) -> std::result::Result<bool, secret_store::SecretError> {
        self.inner.delete(name)
    }
    fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
        self.inner.list_names()
    }
}

/// Assemble the loop (tools + providers) over `storage` with only the built-in
/// key-shape redaction (see [`build_agent_with_redactor`]).
///
/// `desktop` is the backend of the desktop tools (`None` = no desktop tools).
/// The loop installs its own permission gate for tool calls made from scripts,
/// and the desktop control gate the daemon bridges to the app
/// (see `daemon_api::ControlHub`).
#[cfg(test)]
pub fn build_agent(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
) -> AgentLoop {
    build_agent_with_redactor(
        storage,
        providers,
        data_dir,
        desktop,
        SharedRedactor::default(),
    )
}

/// [`build_agent`] with `redactor` masking secrets before they reach the model,
/// the stored events and the event stream.
#[cfg(test)]
pub fn build_agent_with_redactor(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
    redactor: SharedRedactor,
) -> AgentLoop {
    build_agent_with_live_views(storage, providers, data_dir, desktop, redactor, None)
}

#[cfg(test)]
pub fn build_agent_with_live_views(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
    redactor: SharedRedactor,
    view_registry: Option<Arc<live_view::TargetRegistry>>,
) -> AgentLoop {
    let skills = skill_index_with_assignments(storage, data_dir);
    build_agent_with_skills(
        storage,
        providers,
        data_dir,
        desktop,
        redactor,
        view_registry,
        false,
        skills,
        None,
        false,
    )
    .expect("test agent skill wiring")
}

/// The skill index every agent loop and `/v1/skills` route of the daemon share:
/// the catalog plus the database-backed per-bot assignment source. This is the
/// only place that builds it, so a bot never sees an unfiltered root index.
pub(crate) fn skill_index_with_assignments(storage: &Arc<Storage>, data_dir: &Path) -> SkillIndex {
    let assignments = Arc::new(daemon_api::SkillAssignments::new(Arc::clone(storage)));
    load_skills(data_dir).with_assignments(assignments as Arc<dyn skills::AssignmentSource>)
}

#[allow(clippy::too_many_arguments)]
fn build_agent_with_skills(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
    redactor: SharedRedactor,
    view_registry: Option<Arc<live_view::TargetRegistry>>,
    dynamic_desktop: bool,
    skills: SkillIndex,
    fill_secrets: Option<Arc<secret_store::FillSecrets>>,
    safe_mode: bool,
) -> Result<AgentLoop> {
    if !skills.has_assignments() {
        return Err("skill index must carry the per-bot assignment source".into());
    }
    // Placeholder: `with_builtin_tools` replaces it with the loop's own gate.
    let gate = ResolverGate::new(
        PermissionResolver::new(),
        Policy::approve_destructive(),
        None,
        InitiatorKind::Human,
    );
    // Storage resolves symlinks and junctions in the data dir once; the file
    // tools' protected-path checks must use that same canonical spelling.
    let mut tools = BuiltinConfig::new(storage.data_dir().to_path_buf(), Arc::new(gate));
    tools.desktop.clone_from(&desktop);
    tools.dynamic_desktop = dynamic_desktop;
    tools.fill_secrets = fill_secrets;
    let secret_requests = secret_requests_enabled(
        &std::fs::read_to_string(data_dir.join("host.toml")).unwrap_or_default(),
        std::env::var("BOT_SECRET_REQUESTS").ok().as_deref(),
    );
    tools.secret_requests = secret_requests;
    tools.provenance = Some(Arc::new(builtin_tools::provenance::StorageProvenance(
        Arc::clone(storage),
    )));
    tools.view_registry = view_registry;
    tools.knowledge = Some(Arc::new(
        builtin_tools::knowledge::LocalKnowledgeBackend::new(Arc::clone(storage)),
    ));
    let host_limits = std::fs::read_to_string(data_dir.join("host.toml")).unwrap_or_default();
    let (max_computer_runs, idle) = computer_limits(
        &host_limits,
        std::env::var("BOT_MAX_COMPUTER_RUNS").ok().as_deref(),
        std::env::var("BOT_BROWSER_IDLE_SECONDS").ok().as_deref(),
    );
    tools.browser_settings.idle_timeout = idle;
    let approval_timeout = approval_timeout(
        std::env::var("BOT_APPROVAL_TIMEOUT_SECONDS")
            .ok()
            .as_deref(),
    );
    // Safe mode: no memory recall or write-back work, and the store is not even opened.
    let memory = if safe_mode {
        None
    } else {
        match agent_core::MemoryConfig::open_default(storage.data_dir()) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("warning: memory store unavailable: {e}");
                None
            }
        }
    };
    // The discovery threshold is `LoopConfig`'s default (40).
    Ok(AgentLoop::new(
        Arc::clone(storage),
        ToolRegistry::new(),
        providers,
        LoopConfig {
            redactor: Some(redactor),
            max_computer_runs,
            approval_timeout,
            secret_requests,
            memory,
            safe_mode,
            knowledge_decider: Some(Arc::new(agent_core::HeuristicKnowledgeDecider::new(
                Arc::clone(storage),
            ))),
            ..LoopConfig::default()
        },
    )
    .with_builtin_tools(tools)
    .with_subagents()
    .with_skills(skills)
    .with_memory_tools()
    .with_workflows(
        WorkflowStore::new(data_dir),
        Handles {
            desktop,
            browser: None,
            tools: ToolRegistry::new(),
        },
    ))
}

#[derive(Default, Deserialize)]
struct ComputerLimitsFile {
    #[serde(default)]
    limits: ComputerLimits,
}
#[derive(Default, Deserialize)]
struct ComputerLimits {
    max_computer_runs: Option<usize>,
    browser_idle_seconds: Option<u64>,
}
/// The owner's switch for `secret_request` (experimental, **off by default**): `host.toml`
/// `[experimental] secret_request = true`, overridden by `BOT_SECRET_REQUESTS`
/// (`on`/`1`/`true`/`yes` or `off`/`0`/`false`/`no`). Off: the tool is not offered to the
/// model and the loop refuses it.
pub(crate) fn secret_requests_enabled(host: &str, env: Option<&str>) -> bool {
    let file = toml::from_str::<ExperimentalFile>(host)
        .unwrap_or_default()
        .experimental
        .secret_request
        .unwrap_or(false);
    match env.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => file,
    }
}

#[derive(Default, Deserialize)]
struct ExperimentalFile {
    #[serde(default)]
    experimental: Experimental,
}
#[derive(Default, Deserialize)]
struct Experimental {
    secret_request: Option<bool>,
}

/// How long an unanswered approval or question waits before it expires
/// (`BOT_APPROVAL_TIMEOUT_SECONDS`; `0` = no limit, unset or invalid = the default).
fn approval_timeout(env: Option<&str>) -> Duration {
    env.and_then(|n| n.trim().parse::<u64>().ok())
        .map_or(agent_core::DEFAULT_APPROVAL_TIMEOUT, Duration::from_secs)
}

fn computer_limits(
    host: &str,
    env_runs: Option<&str>,
    env_idle: Option<&str>,
) -> (usize, Duration) {
    let config = toml::from_str::<ComputerLimitsFile>(host)
        .unwrap_or_default()
        .limits;
    let runs = env_runs
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .or(config.max_computer_runs.filter(|n| *n > 0))
        .unwrap_or_else(|| {
            browser_cdp::host::default_computer_runs(browser_cdp::host::total_ram_bytes())
        })
        .min(64);
    let idle = env_idle
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .or(config.browser_idle_seconds.filter(|n| *n > 0))
        .unwrap_or(120);
    (runs, Duration::from_secs(idle))
}

/// Shared/bundled index and per-bot roots. Tool calls resolve their caller's
/// bot directory; a bot-less caller cannot write a learned skill.
#[must_use]
pub fn load_skills(data_dir: &Path) -> SkillIndex {
    SkillIndex::load_roots(SkillRoots::from_data_dir(data_dir))
}

pub use crate::host_runtime::local_desktop;

/// Environment variable with extra allowed browser origins (comma separated).
pub const ALLOWED_ORIGINS_ENV: &str = "BOT_ALLOWED_ORIGINS";

/// Non-empty, trimmed entries of a comma separated origin list.
fn parse_origins(list: &str) -> Vec<String> {
    list.split(',')
        .map(|o| o.trim().trim_end_matches('/'))
        .filter(|o| !o.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Resolves on the first stop request: Ctrl-C everywhere, `SIGTERM` on Unix,
/// console close / logoff / system shutdown on Windows (the OS only allows a few
/// seconds after those, so a long grace period cannot always be honoured there).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = async {
                match term.as_mut() {
                    Some(t) => { t.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
        // A signal that cannot be registered simply never fires.
        macro_rules! wait {
            ($s:expr) => {
                async {
                    match $s {
                        Ok(mut s) => {
                            s.recv().await;
                        }
                        Err(_) => std::future::pending::<()>().await,
                    }
                }
            };
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = wait!(ctrl_close()) => {}
            () = wait!(ctrl_break()) => {}
            () = wait!(ctrl_logoff()) => {}
            () = wait!(ctrl_shutdown()) => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Filename of the email connector configuration in `<data_dir>`.
pub const EMAIL_FILE: &str = "email.toml";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfigFile {
    #[serde(default, alias = "account")]
    pub accounts: Vec<connector_email::AccountConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProviderEmailSection {
    #[serde(default)]
    email: Option<EmailConfigFile>,
}

/// Load email accounts from `<data_dir>/email.toml` (or fallback to `[email]` in `providers.toml`).
#[must_use]
pub fn load_email_accounts(data_dir: &Path) -> Vec<connector_email::AccountConfig> {
    let email_path = data_dir.join(EMAIL_FILE);
    if email_path.exists() {
        match std::fs::read_to_string(&email_path) {
            Ok(s) => match toml::from_str::<EmailConfigFile>(&s) {
                Ok(cfg) => return cfg.accounts,
                Err(e) => eprintln!("warning: {}: {e}", email_path.display()),
            },
            Err(e) => eprintln!("warning: cannot read {}: {e}", email_path.display()),
        }
    }
    let providers_path = provider_catalog::config_path(data_dir);
    if providers_path.exists()
        && let Ok(s) = std::fs::read_to_string(&providers_path)
        && let Ok(cfg) = toml::from_str::<ProviderEmailSection>(&s)
        && let Some(email_cfg) = cfg.email
    {
        return email_cfg.accounts;
    }
    Vec::new()
}

/// Give bots the `schedule_*` tools (limits from `<data_dir>/schedule.toml`).
/// Create / update / resume always ask the user. A `schedule.toml` that is
/// broken or nonsensical disables the tools (fail closed): falling back to the
/// defaults would silently undo limits the user tightened.
fn register_schedule_tools(data_dir: &Path, state: &AppState) {
    match daemon_api::load_schedule_limits(data_dir) {
        Err(e) => eprintln!("warning: schedule tools are disabled: {e}"),
        Ok(limits) => match state.enable_schedule_tools(limits) {
            Ok(n) if n == builtin_tools::SCHEDULE_TOOL_NAMES.len() => {}
            Ok(n) => eprintln!("warning: only {n} schedule tool(s) registered (name clash)"),
            Err(e) => eprintln!("warning: schedule tools are disabled: {e}"),
        },
    }
}

/// Give bots that have the "Can create bots" switch the `bot_*` tools. They
/// are hidden from every other bot and always ask the user.
fn register_bot_tools(state: &AppState) {
    let n = state.enable_bot_tools();
    if n != builtin_tools::BOT_TOOL_NAMES.len() {
        eprintln!("warning: only {n} bot tool(s) registered (name clash)");
    }
}

/// Filename of the optional web tools configuration in `<data_dir>`.
pub const WEB_FILE: &str = "web.toml";

/// Largest `max_bytes` a `web.toml` may set (64 MiB).
const WEB_MAX_BYTES_CAP: usize = 64 << 20;
/// Upper bounds for `web.toml` timeouts, in seconds.
const WEB_TIMEOUT_CAP_SECS: u64 = 120;
const WEB_CONNECT_TIMEOUT_CAP_SECS: u64 = 30;

/// The interface the daemon API listens on (loopback only, never another).
const DAEMON_BIND_IP: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// The daemon's own listener, to be refused by the web tools. The exact port
/// is protected (not the whole IP) so a local `SearXNG` on another loopback
/// port keeps working. Port `0` (ephemeral, real port unknown yet) protects
/// every port of the bind IP.
#[must_use]
pub fn daemon_protected_sockets(daemon_port: u16) -> Vec<SocketAddr> {
    vec![SocketAddr::from((DAEMON_BIND_IP, daemon_port))]
}

/// `<data_dir>/web.toml`. Every key is optional; the web tools work without
/// the file (keyless fallbacks need no key).
///
/// ```toml
/// [search]
/// searxng_url = "http://127.0.0.1:8080"  # trusted local/private instance
/// brave_key = "web/brave"                # NAME of a secret, never the key
/// tavily_key = "web/tavily"              # NAME of a secret, never the key
/// duckduckgo = true                      # keyless fallback (default)
/// duckduckgo_lite = true                 # keyless fallback (default)
/// bing = true                            # keyless fallback (default)
/// order = ["searxng", "brave", "tavily", "duckduckgo", "duckduckgo_lite", "bing"]
///
/// [http]
/// timeout_secs = 20
/// connect_timeout_secs = 8
/// max_bytes = 5242880
/// max_redirects = 5
/// ```
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebConfigFile {
    #[serde(default)]
    pub search: WebSearchSection,
    #[serde(default)]
    pub http: WebHttpSection,
}

/// `[search]` of [`WebConfigFile`].
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebSearchSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searxng_url: Option<String>,
    /// Name of the secret holding the Brave Search API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brave_key: Option<String>,
    /// Name of the secret holding the Tavily Search API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavily_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duckduckgo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duckduckgo_lite: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// `[http]` of [`WebConfigFile`].
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebHttpSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<usize>,
}

/// Read `<data_dir>/web.toml`. A missing file is the default; an unreadable or
/// malformed one yields the default plus a warning (it never stops the daemon).
#[must_use]
pub fn load_web_file(data_dir: &Path) -> (WebConfigFile, Vec<String>) {
    let path = data_dir.join(WEB_FILE);
    if !path.exists() {
        return (WebConfigFile::default(), Vec::new());
    }
    let parsed = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
        .and_then(|s| {
            toml::from_str::<WebConfigFile>(&s).map_err(|e| format!("{}: {e}", path.display()))
        });
    match parsed {
        Ok(f) => (f, Vec::new()),
        Err(e) => (
            WebConfigFile::default(),
            vec![format!("{e}; using the default web tools settings")],
        ),
    }
}

/// Build the [`web_tools::WebToolsConfig`] for `file`. Invalid single values are
/// dropped with a warning in `warnings`; the SSRF policy is always the default.
#[must_use]
pub fn web_tools_config(
    data_dir: &Path,
    file: &WebConfigFile,
    store: Option<&Arc<dyn SecretStore>>,
    protected: &[SocketAddr],
    warnings: &mut Vec<String>,
) -> web_tools::WebToolsConfig {
    let mut cfg = web_tools::WebToolsConfig {
        policy: Arc::new(web_tools::DefaultUrlPolicy {
            blocked_sockets: protected.to_vec(),
            ..web_tools::DefaultUrlPolicy::default()
        }),
        secrets: store.cloned(),
        spill_dir: Some(data_dir.join("spill")),
        ..web_tools::WebToolsConfig::default()
    };
    let s = &file.search;
    cfg.search.searxng_url = s.searxng_url.clone().filter(|u| !u.trim().is_empty());
    if let Some(d) = s.duckduckgo {
        cfg.search.duckduckgo = d;
    }
    if let Some(d) = s.duckduckgo_lite {
        cfg.search.duckduckgo_lite = d;
    }
    if let Some(b) = s.bing {
        cfg.search.bing = b;
    }
    if let Some(order) = &s.order {
        for name in order {
            if web_tools::KNOWN_BACKENDS.contains(&name.as_str()) {
                if !cfg.search.order.contains(name) {
                    cfg.search.order.push(name.clone());
                }
            } else {
                warnings.push(format!(
                    "web.toml: unknown search backend {name:?} in order; ignored"
                ));
            }
        }
    }
    let resolve_key = |field: &str,
                       raw: Option<&str>,
                       warnings: &mut Vec<String>|
     -> Option<secret_store::SecretRef> {
        let name = raw.filter(|n| !n.trim().is_empty())?;
        match secret_store::SecretRef::new(name) {
            Ok(r) if store.is_some() => Some(r),
            Ok(_) => {
                warnings.push(format!(
                    "web.toml: {field} {name:?} ignored, the secret store is not available"
                ));
                None
            }
            Err(e) => {
                warnings.push(format!("web.toml: {field} ignored: {e}"));
                None
            }
        }
    };
    cfg.search.brave_key = resolve_key("brave_key", s.brave_key.as_deref(), warnings);
    cfg.search.tavily_key = resolve_key("tavily_key", s.tavily_key.as_deref(), warnings);
    let h = &file.http;
    let positive = |what: &str, v: Option<u64>, warnings: &mut Vec<String>| {
        v.filter(|n| {
            let ok = *n > 0;
            if !ok {
                warnings.push(format!("web.toml: {what} must be greater than 0; ignored"));
            }
            ok
        })
    };
    if let Some(n) = positive("timeout_secs", h.timeout_secs, warnings) {
        if n > WEB_TIMEOUT_CAP_SECS {
            warnings.push(format!(
                "web.toml: timeout_secs capped at {WEB_TIMEOUT_CAP_SECS}"
            ));
        }
        cfg.http.timeout = Duration::from_secs(n.min(WEB_TIMEOUT_CAP_SECS));
    }
    if let Some(n) = positive("connect_timeout_secs", h.connect_timeout_secs, warnings) {
        if n > WEB_CONNECT_TIMEOUT_CAP_SECS {
            warnings.push(format!(
                "web.toml: connect_timeout_secs capped at {WEB_CONNECT_TIMEOUT_CAP_SECS}"
            ));
        }
        cfg.http.connect_timeout = Duration::from_secs(n.min(WEB_CONNECT_TIMEOUT_CAP_SECS));
    }
    if let Some(n) = h.max_bytes {
        if n == 0 {
            warnings.push("web.toml: max_bytes must be greater than 0; ignored".into());
        } else if n > WEB_MAX_BYTES_CAP {
            warnings.push(format!(
                "web.toml: max_bytes capped at {WEB_MAX_BYTES_CAP} bytes"
            ));
            cfg.http.max_bytes = WEB_MAX_BYTES_CAP;
        } else {
            cfg.http.max_bytes = n;
        }
    }
    if let Some(n) = h.max_redirects {
        cfg.http.max_redirects = n.min(10);
    }
    cfg
}

/// Which search backends `cfg` will try, in order (for logs and `bot doctor`).
#[must_use]
pub fn web_backend_summary(cfg: &web_tools::WebToolsConfig) -> String {
    let mut labels = std::collections::HashMap::new();
    if let Some(u) = &cfg.search.searxng_url {
        // Only scheme://host[:port]: never a path, query or credentials.
        let origin = url::Url::parse(u)
            .ok()
            .and_then(|p| {
                let host = p.host_str()?;
                let port = p.port().map_or_else(String::new, |n| format!(":{n}"));
                Some(format!("{}://{host}{port}", p.scheme()))
            })
            .unwrap_or_else(|| "invalid url".to_owned());
        labels.insert("searxng", format!("searxng ({origin})"));
    }
    if cfg.search.brave_key.is_some() {
        labels.insert("brave", "brave".to_owned());
    }
    if cfg.search.tavily_key.is_some() {
        labels.insert("tavily", "tavily".to_owned());
    }
    if cfg.search.duckduckgo {
        labels.insert("duckduckgo", "duckduckgo".to_owned());
    }
    if cfg.search.duckduckgo_lite {
        labels.insert("duckduckgo_lite", "duckduckgo_lite".to_owned());
    }
    if cfg.search.bing {
        labels.insert("bing", "bing".to_owned());
    }
    let mut v = Vec::new();
    for name in &cfg.search.order {
        if let Some(label) = labels.remove(name.as_str()) {
            v.push(label);
        }
    }
    for name in web_tools::KNOWN_BACKENDS {
        if let Some(label) = labels.remove(name) {
            v.push(label);
        }
    }
    if v.is_empty() {
        "none".to_owned()
    } else {
        v.join(" > ")
    }
}

/// Register the web tools (`web_search`, `web_extract`, ...) into `registry`.
///
/// On by default: no configuration is needed. A broken `web.toml`, or one that
/// cannot be turned into a working configuration, logs a warning and falls back
/// to the defaults. Returns how many tools were added.
pub fn register_web(
    data_dir: &Path,
    daemon_port: u16,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> usize {
    let (file, mut warnings) = load_web_file(data_dir);
    let protected = daemon_protected_sockets(daemon_port);
    let cfg = web_tools_config(data_dir, &file, store, &protected, &mut warnings);
    let mut summary = web_backend_summary(&cfg);
    let mut result = builtin_tools::register_web_tools(registry, cfg);
    if let Err(e) = &result {
        warnings.push(format!(
            "web.toml: {e}; using the default web tools settings"
        ));
        let cfg = web_tools_config(
            data_dir,
            &WebConfigFile::default(),
            store,
            &protected,
            &mut warnings,
        );
        summary = web_backend_summary(&cfg);
        result = builtin_tools::register_web_tools(registry, cfg);
    }
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    match result {
        Ok(added) => {
            eprintln!(
                "web: {} tool(s) registered (search: {summary})",
                added.len()
            );
            added.len()
        }
        Err(e) => {
            eprintln!("warning: web tools unavailable: {e}");
            0
        }
    }
}

/// How `bot serve` runs.
#[allow(clippy::struct_excessive_bools)] // independent start-up switches, one per CLI flag
pub struct ServeOptions {
    /// Use the host pairing role and default peer networking on.
    pub host_mode: bool,
    /// Explicitly enable peer networking.
    pub peer_enabled: bool,
    /// Loopback TCP port.
    pub port: u16,
    /// Machine-readable startup / stop lines.
    pub json: bool,
    /// Grace period of running tool steps on shutdown.
    pub grace: Duration,
    /// Secret store of the Telegram channel (`None` = no channels, e.g. the
    /// fake-provider demo).
    pub telegram_store: Option<Arc<dyn SecretStore>>,
    /// Owner key handed over by the desktop app on stdin (never argv, a file
    /// or the environment). A host without one creates the `owner-key` file.
    pub owner_key: Option<String>,
    /// Safe mode: no tools for the model, no scheduler, channels, MCP servers,
    /// apps, memory work, peer networking or desktop; nothing stored is changed.
    pub safe_mode: bool,
}

/// What `GET /v1/doctor` learns from `bot serve`: the desktop backend's
/// capabilities and the production secret redactor (for the support report).
/// Browsers are launched per run by the browser tools, so there is no shared
/// session to restart (the repair stays unavailable).
struct ServeDoctorHost {
    desktop: Option<desktop_api::Capabilities>,
    redactor: SharedRedactor,
}

impl daemon_api::DoctorHost for ServeDoctorHost {
    fn desktop(&self) -> Option<desktop_api::Capabilities> {
        self.desktop.clone()
    }

    fn redactor(&self) -> Option<SharedRedactor> {
        Some(self.redactor.clone())
    }
}

/// Start-up progress, one line per step with the milliseconds since the first one, so the
/// daemon log of a machine where `serve` stops early says exactly where it stopped.
fn step(name: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    eprintln!("startup: {name} (+{} ms)", start.elapsed().as_millis());
}

/// Run the daemon until a stop request (see [`shutdown_signal`]); running tool
/// steps get `grace` to finish, everything else live is paused for the next start.
///
/// # Errors
/// Storage, token, bind or server errors.
#[allow(clippy::too_many_lines)] // daemon assembly and ordered lifecycle in one place
pub fn serve(
    data_dir: &Path,
    opts: &ServeOptions,
    providers: Providers,
    host: Option<ProviderHost>,
    desktop: Option<Arc<dyn DesktopBackend>>,
) -> Result<()> {
    let ServeOptions {
        port,
        json,
        grace,
        telegram_store,
        host_mode,
        peer_enabled,
        owner_key,
        safe_mode,
    } = opts;
    let safe_mode = *safe_mode;
    let (port, json, grace) = (*port, *json, *grace);
    step("begin");
    let mut ownership = daemon_api::host::HostConfig::load(data_dir)?;
    if *host_mode {
        ownership.mode = daemon_api::host::HostMode::Host;
        if !data_dir.join("host.toml").exists() {
            ownership.state = if data_dir.join("bot.db").exists() {
                daemon_api::host::HostState::Active
            } else {
                daemon_api::host::HostState::Fresh
            };
        }
        ownership.save(data_dir)?;
    }
    let storage = Arc::new(Storage::open(data_dir)?);
    step("database open");
    // Result of the early process guard (see `run_serve`), for `doctor`.
    if let Err(e) = process_guard::record(data_dir, &process_guard::protect()) {
        eprintln!("warning: could not record the process guard status: {e}");
    }
    let token = load_or_create_token(data_dir)?;
    step("api token ready");
    if !token.restricted {
        eprintln!(
            "warning: could not restrict {} to the current user",
            token_path(data_dir).display()
        );
    }
    let api_token = token.token;
    let passphrase = env_passphrase();
    let sources = RedactorSources {
        store: telegram_store.clone(),
        api_token: api_token.clone(),
        passphrase,
        catalog_vars: catalog_env_vars(data_dir),
    };
    let redactor = production_redactor(&sources);
    // Secrets written through the daemon are masked immediately.
    let telegram_store: Option<Arc<dyn SecretStore>> = telegram_store.as_ref().map(|s| {
        Arc::new(RegisteringStore {
            inner: Arc::clone(s),
            redactor: redactor.clone(),
        }) as Arc<dyn SecretStore>
    });
    let desktop_caps = desktop.as_ref().map(|d| d.capabilities());
    step("desktop backend capabilities read");
    let views = Arc::new(live_view::TargetRegistry::default());
    // This computer's screen becomes a (lazy) view target. Opening it can touch
    // the OS, so it is probed on a worker thread with a deadline: the API starts
    // regardless, and the panel is told why while there is no screen. The raw
    // reason goes only to this log, redacted.
    let _desktop_view = crate::view_targets::DesktopRegistrar::start(
        Arc::clone(&views),
        crate::view_targets::local_opener(desktop.clone()),
        {
            let redactor = redactor.clone();
            Arc::new(move |line: &str| eprintln!("warning: {}", redactor.redact(line)))
        },
        crate::view_targets::Timing::default(),
    );
    let host_is_host = ownership.mode == daemon_api::host::HostMode::Host;
    // Safe mode never starts peer networking, whatever host.toml / config.toml say.
    let effective_peer_enabled =
        !safe_mode && peer_enabled_for_startup(data_dir, host_is_host, *peer_enabled, *host_mode)?;
    let dynamic_desktop = effective_peer_enabled;
    let local_desktop = desktop.clone();
    let skill_index = skill_index_with_assignments(&storage, data_dir);
    step("skill index built");
    // Owner-stored fill secrets live in the same secret store as everything
    // else, under `fill/`; every value is registered with the production
    // redactor as it is stored and as it is read, on top of the periodic
    // refresh that already covers every name in the store.
    let fill_secrets = telegram_store.as_ref().and_then(|store| {
        let hook_redactor = redactor.clone();
        match secret_store::FillSecrets::open(Arc::clone(store), data_dir) {
            Ok(f) => Some(Arc::new(f.with_value_hook(Arc::new(move |v: &str| {
                hook_redactor.register(v);
            })))),
            Err(e) => {
                eprintln!("warning: fill secrets are disabled: {e}");
                None
            }
        }
    });
    let agent = build_agent_with_skills(
        &storage,
        providers,
        data_dir,
        desktop,
        redactor.clone(),
        Some(Arc::clone(&views)),
        dynamic_desktop,
        skill_index.clone(),
        fill_secrets.clone(),
        safe_mode,
    )?;
    step("agent built");
    // Same registry the loop calls tools from: MCP servers, email and web tools register into it.
    let mcp_tools = agent.tool_registry();
    // The user's own computer as a per-request target of the bots living on this
    // server (card 272): a separate `pc_*` family, bound to the paired desktop
    // once the peer stack below exists. Only a host has bots to serve.
    let pc_slot = Arc::new(builtin_tools::pc::PcLinkSlot::new());
    if host_is_host && !safe_mode {
        // The bot's name goes on the computer's cards.
        let names_storage = Arc::clone(&storage);
        let names: builtin_tools::pc::BotNames = Arc::new(move |id: &str| {
            let id = uuid::Uuid::parse_str(id).ok()?;
            names_storage
                .get_bot(protocol::BotId(id))
                .ok()
                .flatten()
                .map(|b| b.name)
        });
        for tool in builtin_tools::pc::pc_tools(
            &(pc_slot.clone() as Arc<dyn builtin_tools::pc::PcLink>),
            data_dir,
            &names,
        ) {
            let _ = mcp_tools.register(tool);
        }
    }
    let composio_tools = mcp_tools.clone();
    // Safe mode registers no connector at all: no email, Home Assistant, calendar or web
    // tools (their settings routes answer "not configured" until a normal start).
    let email = if safe_mode {
        None
    } else {
        let email = crate::email_host::start_email(data_dir, &mcp_tools, telegram_store.as_ref())
            .map(|(host, _)| host);
        crate::homeassistant_host::start_homeassistant(
            data_dir,
            &mcp_tools,
            telegram_store.as_ref(),
        );
        crate::calendar_host::start_calendar(data_dir, &mcp_tools, telegram_store.as_ref());
        register_web(data_dir, port, &mcp_tools, telegram_store.as_ref());
        email
    };
    step("web tools registered");
    let web_host =
        crate::web_host::WebHost::new(data_dir, port, mcp_tools.clone(), telegram_store.clone());
    let recovered = agent.recover()?;
    step("interrupted runs recovered");
    let mut cfg = DaemonConfig::new(api_token.clone());
    cfg.shutdown_grace = grace;
    cfg.safe_mode = safe_mode;
    // Extra browser origins (e.g. the desktop app's dev server), comma separated.
    if let Ok(extra) = std::env::var(ALLOWED_ORIGINS_ENV) {
        cfg.allowed_origins.extend(parse_origins(&extra));
    }
    let state = AppState::with_view_registry(agent.clone(), storage, cfg, views)?;
    step("app state built");
    // The owner key: handed over by the desktop app on stdin (memory only), else
    // the `owner-key` file of this data dir (0600) for a terminal `bot serve`,
    // a host and the `bot` commands the owner runs in a terminal (card 262).
    if let Some(key) = owner_key.clone() {
        state.set_owner_key(key);
    } else {
        match daemon_api::load_or_create_owner_key(data_dir) {
            Ok(key) => state.set_owner_key(key),
            Err(e) => {
                eprintln!("warning: owner key unavailable, owner-only operations are off: {e}");
            }
        }
    }
    state
        .set_skills(skill_index)
        .map_err(|e| format!("skills wiring: {e}"))?;
    if !safe_mode {
        register_schedule_tools(data_dir, &state);
        register_bot_tools(&state);
    }
    step("schedule tools registered");
    if let Some(f) = &fill_secrets {
        state.set_fill_secrets(Arc::clone(f));
    }
    state.set_doctor_host(Arc::new(ServeDoctorHost {
        desktop: desktop_caps,
        redactor: redactor.clone(),
    }));
    if let Some(host) = host {
        state.set_provider_host(host);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    step("runtime built");
    rt.block_on(async {
        // Backstop poll: pick up secrets stored behind the daemon's back (the CLI,
        // the environment).  Runs with or without a secret store.
        let refresher = {
            let handle = redactor.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(REDACTOR_REFRESH_INTERVAL);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    let h = handle.clone();
                    let _ = tokio::task::spawn_blocking(move || h.refresh_now()).await;
                }
            })
        };
        // This computer as a target for a paired host's bots (card 258-A): the same
        // desktop RPC a bot-node serves, behind the owner's per-host switch.
        let pc_target = (!host_is_host).then(|| {
            let access = Arc::new(crate::pc_target::AccessStore::open(data_dir));
            let requests = Arc::new(daemon_api::pc_access::PcRequests::new());
            let handler = local_desktop.clone().map(|backend| {
                let audit = state.audit_writer_as("pc-target");
                let hub = state.control_hub();
                let live_hub = state.control_hub();
                let masker = redactor.clone();
                Arc::new(crate::pc_target::HostDesktopHandler::new(
                    crate::pc_target::PcTargetDeps {
                        raw: backend,
                        data: data_dir.join(""),
                        access: access.clone(),
                        requests: requests.clone(),
                        gate: hub,
                        owner_ui_live: Arc::new(move || live_hub.owner_ui_live()),
                        audit: Arc::new(move |action, detail| audit(action, &detail)),
                        redact: Arc::new(move |text| masker.redact(text)),
                        own_pid: std::process::id(),
                        tools: builtin_tools::pc::local_tools(data_dir),
                        browser: Some({
                            // This computer's managed browser, one per bot of
                            // the server: a window, a fresh isolated profile
                            // each, never the owner's own.
                            let data_dir = data_dir.to_path_buf();
                            Arc::new(move || {
                                let mut settings =
                                    builtin_tools::BrowserSettings::isolated(&data_dir);
                                settings.headless = false;
                                Arc::new(builtin_tools::CdpDriver::new(settings))
                                    as Arc<dyn builtin_tools::BrowserDriver>
                            })
                        }),
                    },
                ))
            });
            crate::host_links::PcTarget {
                access,
                requests,
                handler,
            }
        });
        // What this daemon serves over RPC: a computer serves its desktop (and,
        // behind the owner's switches, commands, files and browser) to its
        // paired server.
        // A host serves its browser (and nothing else) to its paired
        // administrator desktop, for bots that live on that desktop and use this
        // server as their computer.
        let rpc_handler: Option<Arc<dyn peer_network::RpcHandler>> = if host_is_host {
            let audit = state.audit_writer_as("server-target");
            let masker = redactor.clone();
            let browser_data = data_dir.to_path_buf();
            Some(Arc::new(crate::server_target::ServerTargetHandler::new(
                crate::server_target::ServerTargetDeps {
                    data: data_dir.to_path_buf(),
                    audit: Arc::new(move |action, detail| audit(action, &detail)),
                    redact: Arc::new(move |text| masker.redact(text)),
                    browser: Some(Arc::new(move || {
                        // This host's managed browser for one bot: a window when
                        // a display exists, else headless; an isolated profile,
                        // never the owner's own.
                        let mut settings = builtin_tools::BrowserSettings::isolated(&browser_data);
                        settings.headless = !browser_cdp::host::has_display();
                        Arc::new(builtin_tools::CdpDriver::new(settings))
                            as Arc<dyn builtin_tools::BrowserDriver>
                    })),
                },
            )))
        } else {
            pc_target
                .as_ref()
                .and_then(|pc| pc.handler.clone())
                .map(|h| h as Arc<dyn peer_network::RpcHandler>)
        };
        step("peer stack starting");
        let peer = if safe_mode {
            None
        } else {
            crate::peer::start_server(
                data_dir,
                host_is_host,
                effective_peer_enabled,
                state.clone(),
                rpc_handler,
            )
            .await?
        };
        step("peer stack handled");
        if let Some(net) = peer.clone() {
            crate::host::watch_peers(&state, net);
        }
        // Screen grants of paired devices survive a restart: a headless host
        // has nobody at its screen to grant them again. Pairing
        // reconciliation below still revokes devices that are gone.
        state.load_screen_privileges().await;
        step("screen privileges loaded");
        if host_is_host {
            // Bots moved here before the import mapped their computer still say "the
            // owner's PC": once, logged, they run on this host's own computer.
            let moved = state.normalise_moved_bots();
            if moved > 0 {
                eprintln!("migration: {moved} moved bot(s) now use this host's own computer");
            }
        }
        if let Some(pc) = pc_target.filter(|_| !safe_mode) {
            // This computer's links to paired hosts (`/v1/hosts`); the peer
            // endpoint starts lazily unless `--peer` already started one.
            let links =
                crate::host_links::install(&state, data_dir.to_path_buf(), peer.clone(), pc);
            // A bot may use a paired server as ITS computer while it runs here.
            agent.set_server_links(Arc::new(crate::host_links::CliServerLinks(links)));
        }
        step("host links installed");
        let local_id = crate::computer::machine_computer_id(data_dir, peer.as_ref());
        step("computer identity read");
        let computer = Arc::new(crate::computer::CliComputerProvider::new(
            host_is_host,
            local_desktop,
            peer.clone(),
            local_id.clone(),
        ));
        agent.set_computer_provider(Arc::clone(&computer) as Arc<dyn agent_core::ComputerProvider>);
        pc_slot.set(Arc::new(crate::computer::CliPcLink::new(Arc::clone(
            &computer,
        ))));
        if crate::computer::is_machine_fingerprint(&local_id) {
            // Without a running peer stack the paired list is unknown, not empty.
            let paired_computers: Option<Vec<String>> = peer.as_ref().map(|net| {
                net.devices()
                    .into_iter()
                    .filter(crate::computer::is_remote_desktop_candidate)
                    .map(|device| device.id)
                    .collect()
            });
            agent.reconcile_computer_identity(&local_id, paired_computers.as_deref())?;
        } else {
            eprintln!(
                "warning: local computer identity unavailable this boot; skipping computer grant reconcile (nothing was changed or persisted)"
            );
        }
        agent.reconcile_waiting_for_pc_runs()?;
        step("computer grants reconciled");
        // A revoked pairing loses screen privileges, live sessions and takeover
        // within a couple of seconds, wherever the revoke happened.
        if let Some(net) = peer.clone() {
            crate::computer::spawn_peer_monitor(
                net.clone(),
                Arc::clone(&computer),
                agent.clone(),
                state.downgrade(),
            );
            let weak = state.downgrade();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    let paired = net
                        .devices()
                        .into_iter()
                        .map(|d| format!("peer:{}", d.id))
                        .collect();
                    let _ = state.reconcile_paired_peers(&paired).await;
                }
            });
        }
        // Connect in the background (a slow `npx` must not delay the listener)
        // and expose `/v1/mcp/*` plus the write-only secrets route.
        // The same live redactor as the agent loop masks MCP diagnostics.
        let mcp_redactor: mcp_bridge::ExternalRedactor = {
            let handle = redactor.clone();
            Arc::new(move |text: &str| handle.redact(text))
        };
        let mcp_registrar: mcp_bridge::oauth::SecretRegistrar = {
            let handle = redactor.clone();
            Arc::new(move |value: &str| handle.register(value))
        };
        step("mcp host loading configuration");
        // Safe mode connects no MCP server and does not even touch `mcp.toml`.
        let mcp = (!safe_mode).then(|| {
            crate::mcp_host::McpHost::start(
                data_dir,
                mcp_tools,
                telegram_store.clone(),
                Some(mcp_redactor),
                Some(mcp_registrar),
            )
        });
        step("mcp host ready, server connections start in background");
        if let Some(mcp) = &mcp {
            state.set_mcp(Arc::clone(mcp) as Arc<dyn McpControl>);
        }
        state.set_components(crate::components_host::ComponentHost::system());
        // Composio app connections: the key stays in the secret store (reserved
        // `composio/` prefix); without a store there is no key custody, so the
        // feature stays off.
        if let Some(store) = telegram_store.clone().filter(|_| !safe_mode) {
            let redact: composio::Redact = {
                let handle = redactor.clone();
                Arc::new(move |text: &str| handle.redact(text))
            };
            let audit = Arc::new(crate::composio_host::DaemonAudit(state.audit_writer()));
            match crate::composio_host::ComposioHost::start(
                data_dir,
                composio_tools,
                store,
                redact,
                audit,
                { let handle = redactor.clone(); Arc::new(move |value| handle.register(value)) },
            ) {
                Ok(host) => state.set_composio(host as Arc<dyn daemon_api::ComposioControl>),
                Err(e) => eprintln!("warning: composio: {e}; app connections are off"),
            }
        }
        if let Some(email) = email {
            state.set_email(email as Arc<dyn daemon_api::EmailControl>);
        }
        if !safe_mode {
            state.set_web(Arc::new(web_host) as Arc<dyn daemon_api::WebControl>);
            state.set_voice(Arc::new(crate::voice_host::VoiceHost::new(
                data_dir,
                port,
                telegram_store.clone(),
            )) as Arc<dyn daemon_api::VoiceControl>);
        }
        if let Some(store) = telegram_store.clone() {
            state.set_secret_store(store);
        }
        let configured = mcp.as_ref().map_or(0, |m| m.list().len());
        if configured > 0 {
            eprintln!("mcp: {configured} server(s) configured");
        }
        step("components and apps wired");
        // Loopback only: never bind another interface.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::from((DAEMON_BIND_IP, port))).await?;
        let addr = listener.local_addr()?;
        step("port bound");
        step("listening");
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "listening": format!("http://{addr}"),
                    "token_file": token_path(data_dir),
                    "recovered_runs": recovered.len(),
                })
            );
        } else {
            println!("listening on http://{addr}");
            println!("token file: {}", token_path(data_dir).display());
            if !recovered.is_empty() {
                println!(
                    "{} interrupted run(s) are paused; resume via POST /v1/runs/{{id}}/resume",
                    recovered.len()
                );
            }
        }
        // MCP children stay up through the agent grace period (a running tool
        // call may still need them) and are stopped once the loop has paused.
        let leader = state.leader().clone();
        let channel_state = state.clone();
        let channel_data = data_dir.to_path_buf();
        let prepare_channels = async move {
            // Safe mode starts no channel: the Telegram bridge stays off.
            if let Some(store) = telegram_store.filter(|_| !safe_mode) {
                let owner_key = channel_state.owner_key();
                let leader = channel_state.leader().clone();
                step("telegram configuration loading");
                let telegram = tokio::task::spawn_blocking(move || {
                    channels::registered_channels(
                        &channel_data,
                        store,
                        &format!("http://{addr}"),
                        &api_token,
                        owner_key.as_deref(),
                        leader,
                    )
                })
                .await;
                match telegram {
                    Ok(channels) => {
                        for channel in channels {
                            channel_state.add_channel(channel);
                        }
                        step("telegram configuration loaded");
                    }
                    Err(e) => eprintln!("warning: telegram configuration task failed: {e}"),
                }
            }
        };
        let shutdown_peer = peer.clone();
        let outcome = daemon_api::serve_with_startup(listener, state, prepare_channels, async move {
            shutdown_signal().await;
            if let Some(peer) = shutdown_peer {
                peer.shutdown().await;
            }
        })
        .await;
        leader.detach_network();
        if let Some(peer) = peer {
            peer.shutdown().await;
        }
        if let Some(mcp) = &mcp {
            mcp.shutdown().await;
        }
        refresher.abort();
        let paused = outcome?;
        if json {
            println!(
                "{}",
                serde_json::json!({ "stopped": true, "paused_runs": paused.len() })
            );
        } else {
            println!("stopped; {} run(s) paused for the next start", paused.len());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

fn peer_enabled_for_startup(
    data_dir: &Path,
    host_is_host: bool,
    peer_enabled: bool,
    host_mode: bool,
) -> Result<bool> {
    crate::peer::is_enabled(data_dir, host_is_host, peer_enabled || host_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email_host::register_email;
    use provider_catalog::ProviderConfig;
    use secret_store::MemoryStore;

    #[test]
    fn host_flag_enables_dynamic_desktop_even_when_peer_config_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[peer]\nenabled = false\n").unwrap();
        assert!(peer_enabled_for_startup(dir.path(), true, false, true).unwrap());
        assert!(!peer_enabled_for_startup(dir.path(), true, false, false).unwrap());
        assert!(peer_enabled_for_startup(dir.path(), false, true, false).unwrap());
    }

    const PRICED: &str = r#"
id = "t"
kind = "open_ai_compatible"
base_url = "http://127.0.0.1:1/v1"
auth = "None"

[capabilities]
tools = true
vision = false
streaming = true
json_mode = false

[compat_flags]

[[models]]
id = "m1"
input_per_mtok = 2.0
output_per_mtok = 8.0
cached_per_mtok = 0.5

[[models]]
id = "unpriced"
"#;

    fn sources(store: Option<Arc<dyn SecretStore>>, passphrase: Option<&str>) -> RedactorSources {
        RedactorSources {
            store,
            api_token: "daemon-token-abcdef123456".into(),
            passphrase: passphrase.map(ToOwned::to_owned),
            catalog_vars: vec!["ACME_KEY".into()],
        }
    }

    struct FixtureTool(tool_registry::ToolSpec);

    #[async_trait::async_trait]
    impl tool_registry::Tool for FixtureTool {
        fn spec(&self) -> &tool_registry::ToolSpec {
            &self.0
        }

        async fn call(
            &self,
            _ctx: &tool_registry::ToolContext,
            _input: serde_json::Value,
        ) -> std::result::Result<tool_registry::ToolOutput, tool_registry::ToolError> {
            Ok(tool_registry::ToolOutput::text("ok"))
        }
    }

    fn register_runtime_egress_fixtures(registry: &tool_registry::ToolRegistry) {
        let config = mcp_bridge::McpServerConfig::http("srv", "http://127.0.0.1:9/mcp");
        let spec = mcp_bridge::registered_spec(
            &config,
            "echo",
            "echo a prompt",
            &serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "hidden_arg": {"type": "string"}
                }
            }),
        );
        assert!(registry.register(std::sync::Arc::new(FixtureTool(spec))));
        let secrets: Arc<dyn secret_store::SecretStore> = Arc::new(MemoryStore::new());
        let openapi = "\
openapi: 3.0.0
info: {title: t, version: '1'}
servers: [{url: 'https://api.example.com'}]
paths:
  /pets:
    get:
      operationId: listPets
      parameters:
        - {name: q, in: query, schema: {type: string}}
      responses: {'200': {description: ok}}
";
        let (_api, imported) = openapi_tools::import_tools(
            openapi,
            &openapi_tools::ImportOptions::default(),
            &openapi_tools::OpenApiConfig::new(secrets),
        )
        .unwrap();
        for tool in imported {
            registry.register(tool);
        }
    }

    #[test]
    fn secret_requests_switch_is_off_by_default_and_reads_file_and_environment() {
        assert!(!secret_requests_enabled("", None));
        assert!(!secret_requests_enabled(
            "[experimental]
secret_request=false",
            None
        ));
        assert!(secret_requests_enabled(
            "[experimental]
secret_request=true",
            None
        ));
        assert!(secret_requests_enabled("", Some(" ON ")));
        assert!(!secret_requests_enabled(
            "[experimental]
secret_request=true",
            Some("off")
        ));
        assert!(secret_requests_enabled(
            "[experimental]
secret_request=true",
            Some("garbage")
        ));
        assert!(!secret_requests_enabled("not toml [", None));
    }

    #[test]
    fn approval_timeout_reads_the_environment() {
        assert_eq!(approval_timeout(None), agent_core::DEFAULT_APPROVAL_TIMEOUT);
        assert_eq!(
            approval_timeout(Some("bad")),
            agent_core::DEFAULT_APPROVAL_TIMEOUT
        );
        assert_eq!(approval_timeout(Some("90")), Duration::from_secs(90));
        assert_eq!(approval_timeout(Some("0")), Duration::ZERO);
    }

    #[test]
    fn computer_limits_default_to_small_host_and_environment_overrides_host_file() {
        assert_eq!(
            computer_limits("", None, None),
            (
                browser_cdp::host::default_computer_runs(browser_cdp::host::total_ram_bytes()),
                Duration::from_secs(120)
            )
        );
        let config = "[limits]\nmax_computer_runs=2\nbrowser_idle_seconds=180\n";
        assert_eq!(
            computer_limits(config, None, None),
            (2, Duration::from_secs(180))
        );
        assert_eq!(
            computer_limits(config, Some("1"), Some("30")),
            (1, Duration::from_secs(30))
        );
        assert_eq!(
            computer_limits("[limits]\nmax_computer_runs=0", Some("0"), Some("bad")),
            (
                browser_cdp::host::default_computer_runs(browser_cdp::host::total_ram_bytes()),
                Duration::from_secs(120)
            )
        );
    }

    // SEC-02 (redaction half): production wiring masks stored secrets, the
    // daemon bearer token and the vault passphrase, and follows later changes.
    fn schedule_state(dir: &Path) -> (AgentLoop, AppState) {
        let storage = Arc::new(Storage::open(dir).unwrap());
        let agent = build_agent_with_redactor(
            &storage,
            Providers::unconfigured(),
            dir,
            None,
            production_redactor(&sources(None, None)),
        );
        let state =
            AppState::new(agent.clone(), storage, DaemonConfig::new("daemon-token")).unwrap();
        (agent, state)
    }

    #[test]
    fn serve_wiring_registers_the_schedule_tools() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, state) = schedule_state(dir.path());
        register_schedule_tools(dir.path(), &state);
        let names = agent.tool_names();
        for n in builtin_tools::SCHEDULE_TOOL_NAMES {
            assert!(names.iter().any(|x| x == n), "{n} missing: {names:?}");
        }
        assert!(
            agent
                .tool_registry()
                .get("schedule_create")
                .unwrap()
                .always_ask()
        );
    }

    #[test]
    fn serve_wiring_registers_the_bot_tools_and_every_changing_one_always_asks() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, state) = schedule_state(dir.path());
        register_bot_tools(&state);
        let names = agent.tool_names();
        for n in builtin_tools::BOT_TOOL_NAMES {
            assert!(names.iter().any(|x| x == n), "{n} missing: {names:?}");
        }
        for n in ["bot_create", "bot_update", "bot_delete"] {
            assert!(agent.tool_registry().get(n).unwrap().always_ask(), "{n}");
        }
    }

    #[test]
    fn a_bad_schedule_toml_registers_no_schedule_tools() {
        for bad in [
            "max_per_bot = \"many\"",
            "max_per_bot = 2
min_interval_secs = 0",
            "timezone = \"Nope/Zone\"",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("schedule.toml"), bad).unwrap();
            let (agent, state) = schedule_state(dir.path());
            register_schedule_tools(dir.path(), &state);
            assert!(
                !agent
                    .tool_names()
                    .iter()
                    .any(|n| n.starts_with("schedule_")),
                "{bad}: tools must stay off"
            );
        }
    }

    #[test]
    fn production_redactor_masks_store_token_and_passphrase() {
        let store = Arc::new(MemoryStore::new());
        store
            .set("provider.key", "stored-provider-secret-1")
            .unwrap();
        let r = production_redactor(&sources(Some(store), Some("vault-passphrase-xyz")));
        let text =
            "a stored-provider-secret-1 b daemon-token-abcdef123456 c vault-passphrase-xyz d";
        assert_eq!(r.redact(text), "a [REDACTED] b [REDACTED] c [REDACTED] d");
        assert!(r.contains_secret("sk-abcdefghijklmnopqrstuvwxyz123456"));
        // Without a store the token is still masked.
        let r = production_redactor(&sources(None, None));
        assert!(r.contains_secret("daemon-token-abcdef123456"));
    }

    #[test]
    fn redactor_refresh_picks_up_a_secret_stored_later() {
        let store = Arc::new(MemoryStore::new());
        let r = production_redactor(&sources(Some(store.clone()), None));
        assert!(!r.contains_secret("late-secret-value-99"));
        store.set("late", "late-secret-value-99").unwrap();
        // The loop calls this at every run start / user message.
        r.refresh_now();
        assert!(r.contains_secret("late-secret-value-99"));
        assert!(r.contains_secret("daemon-token-abcdef123456"));
    }

    // Review #1: provider keys that only live in the environment and match no
    // built-in key shape.
    #[test]
    fn env_only_provider_keys_are_collected() {
        let vars = vec![
            ("ACME_KEY".to_owned(), "acme-plain-key-777".to_owned()),
            ("OTHER_API_KEY".to_owned(), "other-plain-key-888".to_owned()),
            ("db_token".to_owned(), "db-plain-token-999".to_owned()),
            ("SHORT_SECRET".to_owned(), "abc".to_owned()),
            ("HOME".to_owned(), "/home/someone/long/path".to_owned()),
        ];
        let got = env_secret_values(vars, &["ACME_KEY".to_owned()]);
        assert_eq!(
            got,
            vec![
                "acme-plain-key-777",
                "other-plain-key-888",
                "db-plain-token-999"
            ]
        );
        // And they end up masked (extras are registered with the redactor).
        let r = SharedRedactor::default();
        r.refresh_from_store(None, &got).unwrap();
        assert!(r.contains_secret("printenv: ACME_KEY=acme-plain-key-777"));
        // Catalog variable names include the built-in providers' key variables.
        let vars = catalog_env_vars(Path::new("."));
        assert!(vars.contains(&"ANTHROPIC_API_KEY".to_owned()), "{vars:?}");
    }

    // Review #6: the secrets route masks a value the moment it is stored, and
    // the redactor needs no secret store to run.
    #[test]
    fn registering_store_masks_written_values_immediately() {
        let inner: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let r = production_redactor(&sources(None, None));
        let store = RegisteringStore {
            inner,
            redactor: r.clone(),
        };
        assert!(!r.contains_secret("just-written-secret-42"));
        store.set("k", "just-written-secret-42").unwrap();
        assert!(r.contains_secret("just-written-secret-42"));
        // Survives a refresh even though the refresh sources do not hold it.
        r.refresh_now();
        assert!(r.contains_secret("just-written-secret-42"));
        assert_eq!(
            store.get("k").unwrap().unwrap().expose(),
            "just-written-secret-42"
        );
    }

    #[test]
    fn fresh_install_builds_unconfigured_agent() {
        let dir = tempfile::tempdir().unwrap();
        let providers = build_providers(
            dir.path(),
            &Catalog::from_entries([]),
            &MemoryStore::new(),
            &mut |_| {},
        )
        .unwrap();
        assert!(providers.setup_required());
        assert_eq!(providers.ids(), Vec::<String>::new());
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let agent = build_agent(&storage, providers, dir.path(), None);
        assert!(agent.providers().setup_required());
    }

    #[test]
    fn cli_only_provider_leaves_setup_required_false_and_sets_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut cli = provider_catalog::ProviderSettings::new("cli:claude", 1);
        cli.cli = Some(provider_catalog::config::CliSettings::Cli {
            engine: provider_catalog::config::CliEngine::Claude,
            command: "claude".into(),
        });
        let mut cfg = ProviderConfig::default();
        cfg.upsert(cli);
        cfg.save(&config_path(dir.path())).unwrap();

        let providers = build_providers(
            dir.path(),
            &Catalog::from_entries([]),
            &MemoryStore::new(),
            &mut |_| {},
        )
        .unwrap();
        assert!(!providers.setup_required());
        assert_eq!(providers.default_id(), "cli:claude");
        assert!(providers.ids().contains(&"cli:claude".to_owned()));
    }

    #[test]
    fn providers_carry_the_catalog_prices_and_a_reload_refreshes_them() {
        let dir = tempfile::tempdir().unwrap();
        let entry: ProviderEntry = toml::from_str(PRICED).unwrap();
        let catalog = Catalog::from_entries([entry]);
        let mut cfg = ProviderConfig::default();
        cfg.upsert(provider_catalog::ProviderSettings::new("t", 0));
        cfg.save(&config_path(dir.path())).unwrap();

        let store = MemoryStore::new();
        let p = build_providers(dir.path(), &catalog, &store, &mut |_| {}).unwrap();
        let price = p.price_for("t/m1").expect("catalog price");
        assert_eq!(
            (
                price.input_per_mtok,
                price.output_per_mtok,
                price.cached_per_mtok
            ),
            (2.0, 8.0, Some(0.5))
        );
        assert!(p.price_for("t/unpriced").is_none());
        assert!(p.price_for("other/m1").is_none());

        // The reload path (`ProviderHost::reload`) calls the same function and
        // the loop swaps the prices in with the providers.
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let agent = build_agent(&storage, p, dir.path(), None);
        assert_eq!(agent.price_for("t/m1").unwrap().output_per_mtok, 8.0);
    }

    #[test]
    fn production_daemon_registry_egress_schemas_are_all_on_cards() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let providers = Providers::single(Arc::new(daemon_api::fake::FakeProvider));
        let agent = build_agent(&storage, providers, dir.path(), None);
        let registry = agent.tool_registry();
        register_web(dir.path(), 0, &registry, None);
        let state =
            AppState::new(agent.clone(), storage, DaemonConfig::new("daemon-token")).unwrap();
        register_schedule_tools(dir.path(), &state);
        // Optional mail tools use the real daemon registration path, with a
        // memory credential store and no connection to either example host.
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        secrets.set("email/work/password", "test-fixture").unwrap();
        std::fs::write(
            dir.path().join(EMAIL_FILE),
            r#"
[[accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"
[accounts.imap]
host = "imap.example.com"
[accounts.smtp]
host = "smtp.example.com"
"#,
        )
        .unwrap();
        assert_eq!(register_email(dir.path(), &registry, Some(&secrets)), 6);
        let audited = agent.audit_egress_card_schemas().unwrap();
        for name in [
            "workflow_run",
            "skill_publish",
            "skill_write",
            "schedule_create",
            "shell_run",
            "script_run",
            "web_search",
            "email_send",
            "email_draft",
        ] {
            assert!(
                audited.iter().any(|item| item == name),
                "missing {name}: {audited:?}"
            );
        }
        assert!(audited.len() > 8, "{audited:?}");
        register_runtime_egress_fixtures(&registry);
        let audited = agent.audit_egress_card_schemas().unwrap();
        for name in ["mcp:srv/echo", "listPets"] {
            assert!(
                audited.iter().any(|item| item == name),
                "missing {name}: {audited:?}"
            );
        }
        // A local MCP server tool is not classified as an executor in this
        // scope (deferred to card 238); the fixture records that choice.
        for name in ["mcp:srv/echo", "listPets"] {
            assert!(
                !registry.get(name).unwrap().spec().content.executes,
                "{name}"
            );
        }
        // Every tool that starts a program is egress, so its card carries the
        // command, and the executors registered here are exactly these.
        let mut executors: Vec<String> = registry
            .list()
            .into_iter()
            .filter(|spec| spec.content.executes)
            .inspect(|spec| assert!(spec.content.egress, "{}", spec.name))
            .map(|spec| spec.name)
            .collect();
        executors.sort();
        assert_eq!(executors, ["script_run", "shell_run"]);
    }

    #[test]
    fn the_agent_has_subagent_and_skill_tools_and_a_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let providers = Providers::single(Arc::new(daemon_api::fake::FakeProvider));
        let agent = build_agent(&storage, providers, dir.path(), None);
        let names = agent.tool_names();
        for want in ["spawn_subagent", "skill_list", "skill_read", "skill_write"] {
            assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
        }
        assert!(
            names.len() <= LoopConfig::default().discovery_threshold,
            "{} built-in tools exceed the direct limit",
            names.len()
        );
        assert!(names.iter().any(|n| n == "workflow_run"), "{names:?}");
        let idx = load_skills(dir.path());
        assert!(idx.write_dir().is_none());
        assert!(idx.for_bot(Some("b1")).unwrap().write_dir().is_some());
        assert!(idx.get("custom-dropdowns").is_some(), "bundled skills load");
    }

    #[test]
    fn origin_list_is_trimmed_and_filtered() {
        assert_eq!(
            parse_origins(" http://localhost:1420/ ,, http://a.test "),
            vec!["http://localhost:1420", "http://a.test"]
        );
        assert_eq!(parse_origins("").len(), 0);
    }

    #[test]
    fn email_accounts_load_from_email_toml_and_providers_toml() {
        let dir = tempfile::tempdir().unwrap();
        // Initially empty
        assert_eq!(load_email_accounts(dir.path()), Vec::new());

        // Test loading from email.toml
        let email_toml = r#"
[[accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"

[accounts.imap]
host = "imap.example.com"
port = 993

[accounts.smtp]
host = "smtp.example.com"
port = 465
security = "implicit"
"#;
        std::fs::write(dir.path().join(EMAIL_FILE), email_toml).unwrap();
        let loaded = load_email_accounts(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "work");
        assert_eq!(loaded[0].address, "alice@example.com");
        assert_eq!(loaded[0].imap.host, "imap.example.com");

        // Remove email.toml and test fallback to providers.toml
        std::fs::remove_file(dir.path().join(EMAIL_FILE)).unwrap();
        let providers_toml = r#"
[email]
[[email.accounts]]
name = "personal"
address = "bob@example.com"
password = "email/personal/password"

[email.accounts.imap]
host = "imap.mail.com"

[email.accounts.smtp]
host = "smtp.mail.com"
"#;
        std::fs::write(config_path(dir.path()), providers_toml).unwrap();
        let loaded_fallback = load_email_accounts(dir.path());
        assert_eq!(loaded_fallback.len(), 1);
        assert_eq!(loaded_fallback[0].name, "personal");
        assert_eq!(loaded_fallback[0].address, "bob@example.com");
    }

    #[test]
    fn email_tools_register_into_tool_registry() {
        use tool_registry::{Effect, PermissionCategory};

        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        store.set("email/work/password", "secret").unwrap();

        let email_toml = r#"
[[accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"

[accounts.imap]
host = "imap.example.com"

[accounts.smtp]
host = "smtp.example.com"
"#;
        std::fs::write(dir.path().join(EMAIL_FILE), email_toml).unwrap();
        let added = register_email(dir.path(), &reg, Some(&store));
        assert_eq!(added, 6);

        let send = reg.get("email_send").expect("email_send registered");
        assert_eq!(send.spec().effect, Effect::Destructive);
        assert_eq!(send.spec().category, PermissionCategory::Messaging);

        let draft = reg.get("email_draft").expect("email_draft registered");
        assert_eq!(draft.spec().effect, Effect::Write);
        assert_eq!(draft.spec().category, PermissionCategory::Messaging);

        let read = reg.get("email_read").expect("email_read registered");
        assert_eq!(read.spec().effect, Effect::Read);
        assert_eq!(read.spec().category, PermissionCategory::Messaging);
    }
    fn web_names(reg: &ToolRegistry) -> Vec<String> {
        web_tools::TOOL_NAMES
            .iter()
            .filter(|n| reg.get(n).is_some())
            .map(|n| (*n).to_owned())
            .collect()
    }

    #[test]
    fn web_tools_register_by_default_without_web_toml() {
        use tool_registry::{Effect, PermissionCategory};

        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        assert_eq!(register_web(dir.path(), 0, &reg, None), 5);
        assert_eq!(web_names(&reg), web_tools::TOOL_NAMES);
        for name in web_tools::TOOL_NAMES {
            let t = reg.get(name).unwrap();
            // Search is Read; fetching a model-chosen URL is Write (asks).
            let want = if *name == "web_search" {
                Effect::Read
            } else {
                Effect::Write
            };
            assert_eq!(t.spec().effect, want, "{name}");
            assert_eq!(t.spec().category, PermissionCategory::Browser, "{name}");
        }
        let mut w = Vec::new();
        let (file, warnings) = load_web_file(dir.path());
        assert_eq!(warnings.len(), 0);
        let cfg = web_tools_config(dir.path(), &file, None, &[], &mut w);
        assert_eq!(w.len(), 0);
        assert!(cfg.search.duckduckgo);
        assert!(cfg.search.duckduckgo_lite);
        assert!(cfg.search.bing);
        assert_eq!(
            web_backend_summary(&cfg),
            "duckduckgo > duckduckgo_lite > bing"
        );
        assert_eq!(cfg.spill_dir, Some(dir.path().join("spill")));
    }

    #[test]
    fn valid_web_toml_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEB_FILE),
            r#"
[search]
searxng_url = "http://127.0.0.1:8080"
brave_key = "web/brave"
duckduckgo = false
duckduckgo_lite = false
bing = false

[http]
timeout_secs = 7
connect_timeout_secs = 3
max_bytes = 1000000
max_redirects = 2
"#,
        )
        .unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        store.set("web/brave", "k").unwrap();
        let (file, warnings) = load_web_file(dir.path());
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut w = Vec::new();
        let cfg = web_tools_config(dir.path(), &file, Some(&store), &[], &mut w);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(
            cfg.search.searxng_url.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(cfg.search.brave_key.as_ref().unwrap().name(), "web/brave");
        assert!(!cfg.search.duckduckgo);
        assert_eq!(cfg.http.timeout, Duration::from_secs(7));
        assert_eq!(cfg.http.connect_timeout, Duration::from_secs(3));
        assert_eq!(cfg.http.max_bytes, 1_000_000);
        assert_eq!(cfg.http.max_redirects, 2);
        assert_eq!(
            web_backend_summary(&cfg),
            "searxng (http://127.0.0.1:8080) > brave"
        );
        let reg = ToolRegistry::new();
        assert_eq!(register_web(dir.path(), 0, &reg, Some(&store)), 5);
    }

    #[test]
    fn malformed_web_toml_falls_back_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        for bad in [
            "this is = = not toml",
            "[search]
unknown_key = 1
",
        ] {
            std::fs::write(dir.path().join(WEB_FILE), bad).unwrap();
            let (file, warnings) = load_web_file(dir.path());
            assert_eq!(file, WebConfigFile::default());
            assert_eq!(warnings.len(), 1, "{bad}");
            assert!(warnings[0].contains("web.toml"), "{warnings:?}");
            let reg = ToolRegistry::new();
            assert_eq!(register_web(dir.path(), 0, &reg, None), 5, "{bad}");
        }
    }

    #[test]
    fn unusable_web_toml_values_do_not_stop_registration() {
        let dir = tempfile::tempdir().unwrap();
        // A bad SearXNG URL fails the backend build: fall back to the defaults.
        std::fs::write(
            dir.path().join(WEB_FILE),
            "[search]
searxng_url = \"not a url\"
",
        )
        .unwrap();
        let reg = ToolRegistry::new();
        assert_eq!(register_web(dir.path(), 0, &reg, None), 5);

        // A Brave key without a secret store is dropped with a warning.
        std::fs::write(
            dir.path().join(WEB_FILE),
            "[search]
brave_key = \"web/brave\"
[http]
timeout_secs = 0
max_bytes = 99999999999
",
        )
        .unwrap();
        let (file, warnings) = load_web_file(dir.path());
        assert_eq!(warnings.len(), 0);
        let mut w = Vec::new();
        let cfg = web_tools_config(dir.path(), &file, None, &[], &mut w);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(cfg.search.brave_key.is_none());
        assert_eq!(cfg.http.timeout, web_tools::HttpConfig::default().timeout);
        assert_eq!(cfg.http.max_bytes, WEB_MAX_BYTES_CAP);
        let reg = ToolRegistry::new();
        assert_eq!(register_web(dir.path(), 0, &reg, None), 5);
    }

    #[test]
    fn web_timeouts_are_clamped_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEB_FILE),
            "[http]
timeout_secs = 100000
connect_timeout_secs = 999
",
        )
        .unwrap();
        let (file, _) = load_web_file(dir.path());
        let mut w = Vec::new();
        let cfg = web_tools_config(dir.path(), &file, None, &[], &mut w);
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(cfg.http.timeout, Duration::from_secs(WEB_TIMEOUT_CAP_SECS));
        assert_eq!(
            cfg.http.connect_timeout,
            Duration::from_secs(WEB_CONNECT_TIMEOUT_CAP_SECS)
        );
    }

    #[test]
    fn backend_summary_shows_only_the_searxng_origin() {
        let mut cfg = web_tools::WebToolsConfig::default();
        cfg.search.searxng_url =
            Some("https://user:pw@searx.example.org:8443/private/path?token=abc".into());
        let s = web_backend_summary(&cfg);
        assert_eq!(
            s,
            "searxng (https://searx.example.org:8443) > duckduckgo > duckduckgo_lite > bing"
        );
        assert!(!s.contains("pw") && !s.contains("token") && !s.contains("private"));
        cfg.search.searxng_url = Some("not a url".into());
        assert!(web_backend_summary(&cfg).contains("invalid url"));
    }

    #[test]
    fn web_toml_honours_custom_order_and_tavily_key() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(secret_store::MemoryStore::new());
        store.set("web/tavily", "tvly-test").unwrap();
        std::fs::write(
            dir.path().join(WEB_FILE),
            "[search]
tavily_key = \"web/tavily\"
duckduckgo_lite = false
order = [\"bing\", \"tavily\", \"bogus\", \"duckduckgo\"]
",
        )
        .unwrap();
        let (file, file_warnings) = load_web_file(dir.path());
        assert!(file_warnings.is_empty(), "{file_warnings:?}");
        let mut warnings = Vec::new();
        let cfg = web_tools_config(dir.path(), &file, Some(&store), &[], &mut warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("bogus"), "{warnings:?}");
        assert_eq!(
            cfg.search
                .tavily_key
                .as_ref()
                .map(secret_store::SecretRef::name),
            Some("web/tavily")
        );
        assert!(!cfg.search.duckduckgo_lite);
        assert_eq!(web_backend_summary(&cfg), "bing > tavily > duckduckgo");
    }

    #[test]
    fn daemon_listener_is_refused_but_other_loopback_ports_stay_possible() {
        let protected = daemon_protected_sockets(47000);
        assert_eq!(protected, vec![SocketAddr::from((DAEMON_BIND_IP, 47000))]);
        // Port 0 protects every port of the bind IP.
        assert_eq!(daemon_protected_sockets(0)[0].port(), 0);

        let dir = tempfile::tempdir().unwrap();
        let mut w = Vec::new();
        // A non-loopback address stands in to show the entry itself refuses it.
        let cfg = web_tools_config(
            dir.path(),
            &WebConfigFile::default(),
            None,
            &[
                "203.0.113.7:0".parse().unwrap(),
                "203.0.113.9:4000".parse().unwrap(),
            ],
            &mut w,
        );
        let check = |u: &str| cfg.policy.check_url(&url::Url::parse(u).unwrap());
        for port in [80, 8080, 47000] {
            let e = check(&format!("http://203.0.113.7:{port}/")).unwrap_err();
            assert!(e.to_string().contains("protected"), "{e}");
        }
        assert!(check("http://203.0.113.9:4000/").is_err());
        assert!(check("http://203.0.113.9:4001/").is_ok());
        assert!(check("http://203.0.113.8/").is_ok());

        // The real daemon address is refused either way (loopback is blocked).
        let cfg = web_tools_config(
            dir.path(),
            &WebConfigFile::default(),
            None,
            &protected,
            &mut w,
        );
        assert!(
            cfg.policy
                .check_url(&url::Url::parse("http://127.0.0.1:47000/").unwrap())
                .is_err()
        );
    }

    /// `serve()` builds its skill index only through
    /// `skill_index_with_assignments`: an assignment source is attached, the
    /// root index refuses unfiltered reads, and a `selected` bot with nothing
    /// assigned does not see a shared skill the user withheld. A refactor that
    /// drops the source fails here (and `AppState::set_skills` / the loop
    /// builder refuse such an index at startup).
    #[test]
    fn serve_skill_index_carries_the_assignment_source_and_filters_bots() {
        use protocol::{BotId, UserId};
        use storage::records::{Bot, User};
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("skills").join("withheld");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            shared.join("SKILL.md"),
            "---
name: withheld
description: d
version: 1
---
body
",
        )
        .unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let user = User {
            id: UserId::new(),
            name: "u".into(),
            created_at_ms: 1,
        };
        let bot = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "b".into(),
            config: serde_json::json!({}),
            created_at_ms: 2,
        };
        storage.insert_user(&user).unwrap();
        storage.insert_bot(&bot).unwrap();

        let index = skill_index_with_assignments(&storage, dir.path());
        assert!(index.has_assignments());
        assert_eq!(index.list(), Vec::new(), "root index must refuse list()");
        let id = bot.id.to_string();
        assert!(index.for_bot(Some(&id)).unwrap().get("withheld").is_some());
        storage
            .set_bot_skill_assignment(bot.id, storage::SkillAssignmentMode::Selected, &[])
            .unwrap();
        // Same index, no explicit bump: the DB generation invalidates the cache.
        assert!(index.for_bot(Some(&id)).unwrap().get("withheld").is_none());

        // The daemon refuses to serve an index without the source.
        let plain = load_skills(dir.path());
        assert!(!plain.has_assignments());
        let result = build_agent_with_skills(
            &storage,
            Providers::unconfigured(),
            dir.path(),
            None,
            SharedRedactor::default(),
            None,
            false,
            plain,
            None,
            false,
        );
        assert!(
            result.is_err(),
            "missing assignment source must return an error"
        );
    }
}
