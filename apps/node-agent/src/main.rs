//! Node-agent entry point (`bot-node`).
//!
//! Runs on every controlled machine and dials out to the core daemon over WebSocket.

fn main() {
    println!("bot-node {}", env!("CARGO_PKG_VERSION"));
}
