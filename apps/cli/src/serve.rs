//! `bot serve`: assemble storage, providers, tools and the agent loop, then
//! run the daemon API (`daemon-api`) until Ctrl-C / SIGTERM / console close.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers, SharedRedactor};
use agent_router::ladder::{GeminiEmbedder, gemini};
use builtin_tools::{BuiltinConfig, ResolverGate};
use daemon_api::{
    AppState, DaemonConfig, McpControl, ProviderHost, load_or_create_token, token_path,
};
use desktop_api::DesktopBackend;
use permission_engine::{PermissionResolver, Policy};
use protocol::InitiatorKind;
use provider_catalog::{
    Auth, Catalog, ChatProvider, ModelInfo, OpenAiCompatConfig, ProviderConfig, ProviderEntry,
    ProviderKind, ProviderSettings, config_path,
};
use secret_store::{PASSPHRASE_ENV, SecretStore};
use serde::Deserialize;
use skills::{SkillIndex, SkillPaths};
use storage::Storage;
use tool_registry::ToolRegistry;
use workflow_replay::{Handles, WorkflowStore};

use crate::Result;
use crate::channels::{self, TelegramRuntime};

/// Providers from `providers.toml`: every enabled one that can be built, the
/// effective default first.
///
/// # Errors
/// Provider configuration cannot be read or parsed.
pub fn build_providers(
    data_dir: &Path,
    catalog: &Catalog,
    store: &dyn SecretStore,
    warn: &mut dyn FnMut(&str),
) -> Result<Providers> {
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    let mut built: Vec<(String, Arc<dyn ChatProvider>, Vec<ModelInfo>)> = Vec::new();
    for s in cfg.providers.iter().filter(|p| p.enabled) {
        match build_one(s, catalog, store) {
            Ok((p, models)) => built.push((s.id.clone(), p, models)),
            Err(e) => warn(&format!("provider {} skipped: {e}", s.id)),
        }
    }
    let pos = built
        .iter()
        .position(|(id, _, _)| Some(id) == default_id.as_ref())
        .unwrap_or(0);
    if built.is_empty() {
        warn("setup required; run `bot setup` or `bot providers add`");
        return Ok(Providers::unconfigured());
    }
    // The catalog's prices go with the providers, so a reload refreshes them
    // too and `/v1/usage` can report `cost_usd`.
    let (id, first, models) = built.remove(pos);
    let mut providers = Providers::single(first).with_catalog_prices(&id, &models);
    for (id, p, models) in built {
        providers = providers.with(p).with_catalog_prices(&id, &models);
    }
    // K2 embeddings (decision ladder): Gemini when a Gemini key is configured.
    if let Some(key) = gemini_key(&cfg, catalog, store) {
        providers = providers.with_embedder(Arc::new(GeminiEmbedder::new(
            key,
            gemini::DEFAULT_EMBED_MODEL,
        )));
    }
    Ok(providers)
}

/// The API key of the first enabled Gemini provider that has one (stored
/// secret or environment variable), for the router's embedder.
fn gemini_key(cfg: &ProviderConfig, catalog: &Catalog, store: &dyn SecretStore) -> Option<String> {
    cfg.providers.iter().filter(|p| p.enabled).find_map(|s| {
        let entry = s.custom.as_ref().or_else(|| catalog.get(&s.id))?;
        if entry.kind != ProviderKind::Gemini {
            return None;
        }
        let key = match (&s.key, &entry.auth) {
            (Some(r), _) => secret_store::resolve(store, r)
                .ok()
                .map(|k| k.expose().to_string()),
            (None, Auth::ApiKeyEnv(var)) => std::env::var(var).ok(),
            (None, Auth::None) => None,
        };
        key.filter(|k| !k.trim().is_empty())
    })
}

fn build_one(
    s: &ProviderSettings,
    catalog: &Catalog,
    store: &dyn SecretStore,
) -> Result<(Arc<dyn ChatProvider>, Vec<ModelInfo>)> {
    let entry: ProviderEntry = match &s.custom {
        Some(c) => c.clone(),
        None => catalog
            .get(&s.id)
            .cloned()
            .ok_or_else(|| format!("{:?} is not in the catalog", s.id))?,
    };
    let provider = Arc::from(s.build_provider(&entry, store, OpenAiCompatConfig::default())?);
    Ok((provider, entry.models))
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
    include_str!("../../../providers/catalog/lmstudio.toml"),
    include_str!("../../../providers/catalog/ollama.toml"),
    include_str!("../../../providers/catalog/openrouter.toml"),
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
pub fn build_agent_with_redactor(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
    redactor: SharedRedactor,
) -> AgentLoop {
    // Placeholder: `with_builtin_tools` replaces it with the loop's own gate.
    let gate = ResolverGate::new(
        PermissionResolver::new(),
        Policy::approve_destructive(),
        None,
        InitiatorKind::Human,
    );
    let mut tools = BuiltinConfig::new(data_dir.to_path_buf(), Arc::new(gate));
    tools.desktop.clone_from(&desktop);
    // The discovery threshold is `LoopConfig`'s default (40).
    AgentLoop::new(
        Arc::clone(storage),
        ToolRegistry::new(),
        providers,
        LoopConfig {
            redactor: Some(redactor),
            ..LoopConfig::default()
        },
    )
    .with_builtin_tools(tools)
    .with_subagents()
    .with_skills(load_skills(data_dir))
    .with_workflows(
        WorkflowStore::new(data_dir),
        Handles {
            desktop,
            browser: None,
            tools: ToolRegistry::new(),
        },
    )
}

/// The skills index of a data dir: the bundled skills plus `<data_dir>/skills/`,
/// which is also where `skill_write` saves what the bot learns.
#[must_use]
pub fn load_skills(data_dir: &Path) -> SkillIndex {
    let dir = data_dir.join("skills");
    SkillIndex::load(SkillPaths {
        user: Some(dir.clone()),
        bot: Some(dir),
    })
}

/// The desktop backend of the real daemon: Windows UI Automation on Windows,
/// none elsewhere (yet).  `BOT_DESKTOP=off` disables it.
#[must_use]
pub fn local_desktop(warn: &mut dyn FnMut(&str)) -> Option<Arc<dyn DesktopBackend>> {
    if matches!(
        std::env::var("BOT_DESKTOP").as_deref(),
        Ok("off" | "0" | "false")
    ) {
        return None;
    }
    #[cfg(windows)]
    match desktop_windows::WindowsBackend::new() {
        Ok(b) => return Some(Arc::new(b)),
        Err(e) => warn(&format!("desktop tools disabled: {e}")),
    }
    #[cfg(not(windows))]
    let _ = warn;
    None
}

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

/// Register e-mail tools into `registry` if accounts are configured and `store` is available.
pub fn register_email(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> usize {
    let accounts = load_email_accounts(data_dir);
    if accounts.is_empty() {
        return 0;
    }
    let Some(store) = store else {
        eprintln!("warning: email accounts configured but secret store is not available");
        return 0;
    };
    match connector_email::EmailConnector::new(accounts, Arc::clone(store)) {
        Ok(conn) => {
            let conn = Arc::new(conn);
            let added = connector_email::register(registry, &conn);
            let count = conn.account_names().len();
            eprintln!("email: {added} tool(s) registered ({count} account(s))");
            added
        }
        Err(e) => {
            eprintln!("warning: cannot initialize email connector: {e}");
            0
        }
    }
}

/// How `bot serve` runs.
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
}

/// Run the daemon on `listener` until `shutdown` resolves, with the Telegram
/// channel (when it runs) stopped first so no message arrives mid-shutdown.
///
/// # Errors
/// Server errors.
pub async fn run_daemon(
    listener: tokio::net::TcpListener,
    state: AppState,
    telegram: Option<Arc<TelegramRuntime>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<Vec<protocol::RunId>> {
    daemon_api::serve(listener, state, async move {
        shutdown.await;
        if let Some(t) = telegram {
            t.shutdown().await;
        }
    })
    .await
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
    } = opts;
    let (port, json, grace) = (*port, *json, *grace);
    let storage = Arc::new(Storage::open(data_dir)?);
    let token = load_or_create_token(data_dir)?;
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
    let agent = build_agent_with_redactor(&storage, providers, data_dir, desktop, redactor.clone());
    // Same registry the loop calls tools from: MCP servers and email register into it.
    let mcp_tools = agent.tool_registry();
    register_email(data_dir, &mcp_tools, telegram_store.as_ref());
    let recovered = agent.recover()?;
    let mut cfg = DaemonConfig::new(api_token.clone());
    cfg.shutdown_grace = grace;
    // Extra browser origins (e.g. the desktop app's dev server), comma separated.
    if let Ok(extra) = std::env::var(ALLOWED_ORIGINS_ENV) {
        cfg.allowed_origins.extend(parse_origins(&extra));
    }
    let state = AppState::new(agent, storage, cfg)?;
    if let Some(host) = host {
        state.set_provider_host(host);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
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
        let peer =
            crate::peer::start_server(data_dir, *host_mode, *peer_enabled, state.clone()).await?;
        // Connect in the background (a slow `npx` must not delay the listener)
        // and expose `/v1/mcp/*` plus the write-only secrets route.
        // The same live redactor as the agent loop masks MCP diagnostics.
        let mcp_redactor: mcp_bridge::ExternalRedactor = {
            let handle = redactor.clone();
            Arc::new(move |text: &str| handle.redact(text))
        };
        let mcp = crate::mcp_host::McpHost::start(
            data_dir,
            mcp_tools,
            telegram_store.clone(),
            Some(mcp_redactor),
        );
        state.set_mcp(Arc::clone(&mcp) as Arc<dyn McpControl>);
        if let Some(store) = telegram_store.clone() {
            state.set_secret_store(store);
        }
        let configured = mcp.list().len();
        if configured > 0 {
            eprintln!("mcp: {configured} server(s) configured");
        }
        // Loopback only: never bind another interface.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
        let addr = listener.local_addr()?;
        // The Telegram channel (when a token is stored and it is enabled) talks
        // to this daemon through its own loopback address.
        let telegram = telegram_store.as_ref().map(|store| {
            let t = channels::start_telegram(
                data_dir,
                Arc::clone(store),
                &format!("http://{addr}"),
                &api_token,
                None,
            );
            state.add_channel(t.clone());
            t
        });
        if let Some(t) = &telegram {
            use daemon_api::ChannelControl;
            let i = t.info();
            if i.running {
                eprintln!(
                    "telegram channel: running ({} paired user(s))",
                    i.paired_users
                );
            }
        }
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
        let shutdown_peer = peer.clone();
        let outcome = run_daemon(listener, state, telegram, async move {
            shutdown_signal().await;
            if let Some(peer) = shutdown_peer {
                peer.shutdown().await;
            }
        })
        .await;
        if let Some(peer) = peer {
            peer.shutdown().await;
        }
        mcp.shutdown().await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use provider_catalog::ProviderConfig;
    use secret_store::MemoryStore;

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

    // SEC-02 (redaction half): production wiring masks stored secrets, the
    // daemon bearer token and the vault passphrase, and follows later changes.
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
    fn providers_carry_the_catalog_prices_and_a_reload_refreshes_them() {
        let dir = tempfile::tempdir().unwrap();
        let entry: ProviderEntry = toml::from_str(PRICED).unwrap();
        let catalog = Catalog::from_entries([entry]);
        let mut cfg = ProviderConfig::default();
        cfg.upsert(ProviderSettings::new("t", 0));
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
        assert!(idx.write_dir().is_some());
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
}
