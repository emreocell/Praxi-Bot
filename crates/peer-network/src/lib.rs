//! Dial-out peer networking: manages WebSocket connections to remote node-agents.
//!
//! Each remote node opens an outbound WebSocket to the core daemon so no
//! inbound port is required; this crate handles authentication and reconnection.
