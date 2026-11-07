//! `bot serve`: assemble storage, providers, tools and the agent loop, then
//! run the daemon API (`daemon-api`) until Ctrl-C / SIGTERM / console close.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers};
use agent_router::ladder::{GeminiEmbedder, gemini};
use builtin_tools::{BuiltinConfig, ResolverGate};
use daemon_api::{AppState, DaemonConfig, ProviderHost, load_or_create_token, token_path};
use desktop_api::DesktopBackend;
use permission_engine::{PermissionResolver, Policy};
use protocol::InitiatorKind;
use provider_catalog::{
    Auth, Catalog, ChatProvider, ModelInfo, OpenAiCompatConfig, ProviderConfig, ProviderEntry,
    ProviderKind, ProviderSettings, config_path,
};
use secret_store::SecretStore;
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
/// No usable provider.
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
        return Err("no usable provider; run `bot setup` or `bot providers add`".into());
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

/// Assemble the loop (tools + providers) over `storage`.
///
/// `desktop` is the backend of the desktop tools (`None` = no desktop tools).
/// The loop installs its own permission gate for tool calls made from scripts,
/// and the desktop control gate the daemon bridges to the app
/// (see `daemon_api::ControlHub`).
pub fn build_agent(
    storage: &Arc<Storage>,
    providers: Providers,
    data_dir: &Path,
    desktop: Option<Arc<dyn DesktopBackend>>,
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
        LoopConfig::default(),
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

/// How `bot serve` runs.
pub struct ServeOptions {
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
    } = opts;
    let (port, json, grace) = (*port, *json, *grace);
    let storage = Arc::new(Storage::open(data_dir)?);
    let agent = build_agent(&storage, providers, data_dir, desktop);
    let recovered = agent.recover()?;
    let token = load_or_create_token(data_dir)?;
    if !token.restricted {
        eprintln!(
            "warning: could not restrict {} to the current user",
            token_path(data_dir).display()
        );
    }
    let api_token = token.token;
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
        let paused = run_daemon(listener, state, telegram, shutdown_signal()).await?;
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
}
