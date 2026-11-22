//! Managed local model: a pinned `llama-server` build plus a curated GGUF model, downloaded
//! with a checksum, run on loopback only, and found afterwards by [`crate::local_detect`] (the
//! `llamacpp` runtime on port 8080).
//!
//! Nothing here talks to the network on its own: callers decide when to download. Every
//! artefact is pinned by URL, size and SHA-256 in [`manifest`], so a changed or tampered file
//! is refused.

pub mod download;
pub mod extract;
pub mod manifest;
pub mod serve;
pub mod trusted;

pub use download::{DownloadError, DownloadSpec, download, download_verified};
pub use extract::{ExtractError, extract_archive, find_binary};
pub use manifest::{
    ArchiveKind, ManagedModel, RuntimeAsset, candidates, managed_model, managed_models,
    runtime_asset,
};
pub use serve::{LLAMA_PORT, ServeError, ServerHandle, server_args, start_server};
pub use trusted::{TrustError, install_verified, verified_binary, verify_install};
