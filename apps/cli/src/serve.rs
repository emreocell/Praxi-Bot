//! `bot serve`: assemble storage, providers, tools and the agent loop, then
//! run the daemon API (`daemon-api`) until Ctrl-C / SIGTERM / console close.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers};
use builtin_tools::{BuiltinConfig, ResolverGate, register_builtin_tools};
use daemon_api::{AppState, DaemonConfig, load_or_create_token, token_path};
use permission_engine::{PermissionResolver, Policy};
use protocol::InitiatorKind;
use provider_catalog::{
    Catalog, ChatProvider, OpenAiCompatConfig, ProviderConfig, ProviderEntry, ProviderSettings,
    config_path,
};
use secret_store::SecretStore;
use storage::Storage;
use tool_registry::ToolRegistry;

use crate::Result;

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
    let mut built: Vec<(String, Arc<dyn ChatProvider>)> = Vec::new();
    for s in cfg.providers.iter().filter(|p| p.enabled) {
        match build_one(s, catalog, store) {
            Ok(p) => built.push((s.id.clone(), p)),
            Err(e) => warn(&format!("provider {} skipped: {e}", s.id)),
        }
    }
    let pos = built
        .iter()
        .position(|(id, _)| Some(id) == default_id.as_ref())
        .unwrap_or(0);
    if built.is_empty() {
        return Err("no usable provider; run `bot setup` or `bot providers add`".into());
    }
    let (_, first) = built.remove(pos);
    let mut providers = Providers::single(first);
    for (_, p) in built {
        providers = providers.with(p);
    }
    Ok(providers)
}

fn build_one(
    s: &ProviderSettings,
    catalog: &Catalog,
    store: &dyn SecretStore,
) -> Result<Arc<dyn ChatProvider>> {
    let entry: ProviderEntry = match &s.custom {
        Some(c) => c.clone(),
        None => catalog
            .get(&s.id)
            .cloned()
            .ok_or_else(|| format!("{:?} is not in the catalog", s.id))?,
    };
    Ok(Arc::from(s.build_provider(
        &entry,
        store,
        OpenAiCompatConfig::default(),
    )?))
}

/// Assemble the loop (tools + providers) over `storage`.
pub fn build_agent(storage: &Arc<Storage>, providers: Providers, data_dir: &Path) -> AgentLoop {
    let tools = ToolRegistry::new();
    // Tool calls made from scripts cannot wait for a human: `Ask` is refused.
    let gate = ResolverGate::new(
        PermissionResolver::new(),
        Policy::approve_destructive(),
        None,
        InitiatorKind::Human,
    );
    let _ = register_builtin_tools(
        &tools,
        BuiltinConfig::new(data_dir.to_path_buf(), Arc::new(gate)),
    );
    AgentLoop::new(Arc::clone(storage), tools, providers, LoopConfig::default())
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

/// Run the daemon until a stop request (see [`shutdown_signal`]); running tool
/// steps get `grace` to finish, everything else live is paused for the next start.
///
/// # Errors
/// Storage, token, bind or server errors.
pub fn serve(
    data_dir: &Path,
    port: u16,
    providers: Providers,
    json: bool,
    grace: Duration,
) -> Result<()> {
    let storage = Arc::new(Storage::open(data_dir)?);
    let agent = build_agent(&storage, providers, data_dir);
    let recovered = agent.recover()?;
    let token = load_or_create_token(data_dir)?;
    if !token.restricted {
        eprintln!(
            "warning: could not restrict {} to the current user",
            token_path(data_dir).display()
        );
    }
    let mut cfg = DaemonConfig::new(token.token);
    cfg.shutdown_grace = grace;
    // Extra browser origins (e.g. the desktop app's dev server), comma separated.
    if let Ok(extra) = std::env::var(ALLOWED_ORIGINS_ENV) {
        cfg.allowed_origins.extend(parse_origins(&extra));
    }
    let state = AppState::new(agent, storage, cfg)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        // Loopback only: never bind another interface.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
        let addr = listener.local_addr()?;
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
        let paused = daemon_api::serve(listener, state, shutdown_signal()).await?;
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
    use super::parse_origins;

    #[test]
    fn origin_list_is_trimmed_and_filtered() {
        assert_eq!(
            parse_origins(" http://localhost:1420/ ,, http://a.test "),
            vec!["http://localhost:1420", "http://a.test"]
        );
        assert_eq!(parse_origins("").len(), 0);
    }
}
