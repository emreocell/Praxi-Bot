//! Node-agent entry point (`bot-node`).
//!
//! Runs on every controlled machine. `bot-node join <bot-pair:...>` pairs it
//! with a core once; `bot-node run` then serves desktop RPC to the paired core
//! and keeps a heartbeat link to it (reconnecting with backoff).
//!
//! # Running as a service (not automated yet)
//!
//! * Windows: `sc.exe create BotNode binPath= "C:\path\bot-node.exe run" start= auto`
//!   (note: UI Automation needs an interactive session, so a logon task such as
//!   `schtasks /create /sc onlogon /tn BotNode /tr "C:\path\bot-node.exe run"` is
//!   usually the right choice for desktop control).
//! * Linux: a `systemd --user` unit with `ExecStart=/path/bot-node run`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use desktop_api::DesktopBackend;
use peer_network::{
    DesktopRpcHandler, HeartbeatConfig, LinkState, PairTicket, PeerConfig, PeerNetwork,
    RelayConfig, Role, desktop::detect_browser,
};

#[derive(Parser)]
#[command(
    name = "bot-node",
    version,
    about = "Node-agent: lets a paired core control this machine"
)]
struct Cli {
    /// Data directory (default: the bot data dir, honouring `BOT_DATA_DIR`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Relay: `default` (public n0 relays), `none` (direct IP only) or a relay URL.
    #[arg(long, global = true, default_value = "default")]
    relay: RelayConfig,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pair with a core using a `bot-pair:...` ticket.
    Join {
        ticket: String,
        /// Name shown on the core (default: the host name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Serve RPC for paired cores and keep the heartbeat link alive.
    Run,
    /// List paired devices.
    Devices,
}

fn peer_dir(cli: &Cli) -> Result<PathBuf, String> {
    let base = cli
        .data_dir
        .clone()
        .or_else(storage::data_dir)
        .ok_or("cannot determine a data directory; pass --data-dir")?;
    Ok(base.join("peer-node"))
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "node".into())
}

fn capabilities() -> Vec<String> {
    let mut caps = vec!["desktop".to_string()];
    if detect_browser() {
        caps.push("browser".into());
    }
    caps
}

#[cfg(windows)]
fn local_backend() -> Result<Arc<dyn DesktopBackend>, String> {
    desktop_windows::WindowsBackend::new()
        .map(|b| Arc::new(b) as Arc<dyn DesktopBackend>)
        .map_err(|e| format!("cannot start the Windows backend: {e}"))
}

#[cfg(not(windows))]
fn local_backend() -> Result<Arc<dyn DesktopBackend>, String> {
    // The Linux backend (desktop-linux) has no implementation yet; macOS neither.
    Err(format!(
        "desktop control is not supported on {} yet",
        std::env::consts::OS
    ))
}

fn config(cli: &Cli, name: String) -> Result<PeerConfig, String> {
    let mut cfg = PeerConfig::new(peer_dir(cli)?);
    cfg.relay = cli.relay.clone();
    cfg.local_role = Role::Node;
    cfg.display_name = name;
    cfg.capabilities = capabilities();
    Ok(cfg)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = real_main(&cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn real_main(cli: &Cli) -> Result<(), String> {
    match &cli.command {
        Command::Join { ticket, name } => {
            let ticket = PairTicket::parse(ticket).map_err(|e| e.to_string())?;
            let cfg = config(cli, name.clone().unwrap_or_else(host_name))?;
            let net = PeerNetwork::start(cfg, None)
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("this node: {}", peer_network::key_to_string(&net.id()));
            eprintln!("confirmation code from the invite: {}", ticket.code);
            let core = net.pair(&ticket).await.map_err(|e| e.to_string())?;
            println!("paired with '{}' ({}) as {}", core.name, core.id, core.role);
            net.shutdown().await;
            Ok(())
        }
        Command::Devices => {
            let net = PeerNetwork::start(config(cli, host_name())?, None)
                .await
                .map_err(|e| e.to_string())?;
            println!("this node: {}", peer_network::key_to_string(&net.id()));
            for d in net.devices() {
                println!(
                    "{}  {}  {}  {}",
                    d.id,
                    d.role,
                    d.name,
                    d.capabilities.join(",")
                );
            }
            net.shutdown().await;
            Ok(())
        }
        Command::Run => {
            let backend = local_backend()?;
            let handler = Arc::new(DesktopRpcHandler::new(backend));
            let net = PeerNetwork::start(config(cli, host_name())?, Some(handler))
                .await
                .map_err(|e| e.to_string())?;
            let cores: Vec<_> = net
                .devices()
                .into_iter()
                .filter(|d| d.role == Role::Desktop)
                .collect();
            if cores.is_empty() {
                net.shutdown().await;
                return Err("no paired core; run `bot-node join <bot-pair:...>` first".into());
            }
            eprintln!(
                "bot-node {} serving as {} ({} paired core(s)); Ctrl+C to stop",
                env!("CARGO_PKG_VERSION"),
                peer_network::key_to_string(&net.id()),
                cores.len()
            );
            let mut links = Vec::new();
            for core in cores {
                let Ok(key) = core.public_key() else { continue };
                let name = core.name.clone();
                links.push(net.spawn_heartbeat(
                    key,
                    HeartbeatConfig::default(),
                    Arc::new(move |state| match state {
                        LinkState::Connected => eprintln!("link to '{name}': connected"),
                        LinkState::Disconnected(why) => {
                            eprintln!("link to '{name}': down ({why}); retrying");
                        }
                        LinkState::Revoked(why) => {
                            eprintln!("link to '{name}': revoked ({why}); not retrying");
                        }
                    }),
                ));
            }
            tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
            for l in links {
                l.abort();
            }
            net.shutdown().await;
            Ok(())
        }
    }
}
