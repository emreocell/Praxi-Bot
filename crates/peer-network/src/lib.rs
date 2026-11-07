//! Peer networking over iroh.
//!
//! Machines are identified by public key and connect directly (or through a
//! relay) after a one-time, invite-based pairing. Three ALPN protocols run on
//! one endpoint:
//!
//! * `bot/pair/1` - pairing with a one-time invite (`bot-pair:<base32>` + code),
//! * `bot/rpc/1` - JSON-RPC 2.0, one request/response per bi-stream, paired keys only,
//! * `bot/events/1` - heartbeats and events pushed by a node, paired keys only.
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
pub use invite::{INVITE_TTL, Invite, PairTicket};
pub use net::{HeartbeatConfig, LinkState, PeerConfig, PeerEvent, PeerNetwork, RelayConfig};
pub use proto::{ALPN_EVENTS, ALPN_PAIR, ALPN_RPC, PROTOCOL_VERSION};
pub use rpc::{RpcClient, RpcHandler};
pub use store::{Device, Role};
