//! Peer networking over iroh.
//!
//! Machines are identified by public key and connect directly (or through a
//! relay) after a one-time, invite-based pairing. Built-in ALPN protocols run on
//! one endpoint:
//!
//! * `bot/pair/1` - pairing with a one-time invite (`bot-pair:<base32>` + code),
//! * `bot/rpc/1` - JSON-RPC 2.0, one request/response per bi-stream, paired keys only,
//! * `bot/events/1` - heartbeats and events pushed by a node, paired keys only.
//!
//! Extra protocols (including [`ALPN_API`]) are registered with
//! [`PeerConfig::extra_protocols`] and [`ProtocolHandler`]. Every extra protocol
//! requires a paired key, and revoke closes both incoming and outgoing links.
//! Roles are `node`, `phone`, `desktop`, and `host`. Invites default to two
//! minutes and support up to 30 minutes; pairing allows five attempts per minute
//! per remote key and eight concurrent handshakes. Unknown secrets never consume
//! invites. Heartbeats optionally carry [`LeaderInfo`]; leader policy belongs to
//! the daemon. Older device files and heartbeats remain readable.
//!
//! [`RemoteDesktop`] implements [`desktop_api::DesktopBackend`] over the RPC
//! link so the agent loop can drive a paired machine like a local one.

pub mod desktop;
pub mod error;
pub mod identity;
pub mod invite;
pub mod net;
pub mod proto;
pub mod rpc;
pub mod store;

pub use desktop::{DesktopRpcHandler, NodeInfo, RemoteDesktop};
pub use error::{PeerError, Result};
pub use identity::{key_from_string, key_to_string};
pub use invite::{INVITE_TTL, Invite, MAX_INVITE_TTL, PairTicket};
pub use net::{
    HeartbeatConfig, LinkState, PeerConfig, PeerEvent, PeerNetwork, ProtocolHandler, RelayConfig,
};
pub use proto::{
    ALPN_API, ALPN_EVENTS, ALPN_PAIR, ALPN_RPC, LeaderInfo, LeaderState, PROTOCOL_VERSION,
};
pub use rpc::{RpcClient, RpcHandler, RpcLimits};
pub use store::{Device, Role};
