//! Test core for manual checks: creates an invite, waits for a `bot-node` to
//! join and send a heartbeat, then snapshots a window on it over the link.
//!
//! ```text
//! BOT_DATA_DIR=/tmp/core cargo run -p peer-network --example core -- [--relay none|default|<url>] [window-title-substring]
//! ```

use std::sync::Arc;
use std::time::Duration;

use desktop_api::{DesktopBackend, render_snapshot};
use peer_network::{PeerConfig, PeerNetwork, RelayConfig, RemoteDesktop, Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut relay = RelayConfig::Default;
    let mut title = "notepad".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--relay" {
            relay = args.next().ok_or("--relay needs a value")?.parse()?;
        } else {
            title = a;
        }
    }
    let base = std::env::var_os("BOT_DATA_DIR")
        .map(std::path::PathBuf::from)
        .ok_or("set BOT_DATA_DIR to an empty scratch directory")?;
    let mut cfg = PeerConfig::new(base.join("peer-core"));
    cfg.relay = relay;
    cfg.display_name = "test-core".into();
    cfg.local_role = Role::Desktop;
    let net = PeerNetwork::start(cfg, None).await?;
    let mut events = net.subscribe();

    let invite = net.create_invite(Role::Node).await;
    println!("{}", invite.qr_terminal()?);
    println!("ticket: {}", invite.ticket);
    println!("code:   {}  (valid {}s)", invite.code, invite.ttl.as_secs());
    println!("waiting for a node to join and send a heartbeat ...");

    let node = loop {
        let ev = tokio::time::timeout(Duration::from_secs(300), events.recv()).await??;
        match ev.event.kind.as_str() {
            "paired" => println!("paired: {}", peer_network::key_to_string(&ev.peer)),
            "heartbeat" => break ev.peer,
            _ => {}
        }
    };
    println!("heartbeat from {}", peer_network::key_to_string(&node));

    let remote = RemoteDesktop::connect(&net, &node).await?;
    println!("node.info: {:?}", remote.node_info());
    let windows = remote.windows().await?;
    println!("{} window(s):", windows.len());
    for w in &windows {
        println!("  {:>10}  {}", w.id, w.title);
    }
    let needle = title.to_lowercase();
    let w = windows
        .iter()
        .find(|w| w.title.to_lowercase().contains(&needle))
        .ok_or_else(|| format!("no window containing '{title}'"))?;
    let snap = remote.snapshot(Some(w.id)).await?;
    println!(
        "--- snapshot of '{}' over the peer link ---",
        snap.window.title
    );
    for line in render_snapshot(&snap).lines().take(40) {
        println!("{line}");
    }
    let _: Arc<dyn DesktopBackend> = Arc::new(remote);
    net.shutdown().await;
    Ok(())
}
