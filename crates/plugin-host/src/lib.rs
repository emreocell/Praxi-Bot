//! Plugin manifest loading, lifecycle management, and isolation for all plugin types.
//!
//! Reads plugin.toml manifests, starts and supervises plugin processes or WASM
//! modules, and brokers capability grants between plugins and the core daemon.
