//! Composio app connections (card 245).
//!
//! Praxi connects the services that are painful to connect itself through
//! Composio's REST API: one API key (the owner's own, in the OS secret store),
//! Composio-hosted consent pages, an open catalog, and per-app tools that are
//! discovered, capped and sorted into read, write, destructive and money
//! classes (money always asks).  Nothing here follows a URL from
//! a response, and nothing exposes Composio's meta, bash, workbench or
//! connect-link tools to the model.

mod catalog;
mod client;
mod curated;
mod error;
mod keys;
pub mod logo;
mod probe;
mod risk;
mod schema;
mod service;
mod state;
mod surface;
mod tool;

pub use catalog::{CATALOG_FILE, CatalogFile, CatalogSource, CatalogView, STALE_AFTER_SECS};
pub use client::{
    API_BASE, API_ORIGIN, Account, Auth, CliPoll, CliSession, ComposioClient, Link, Redact,
    RemoteTool, Toolkit, ToolkitLookup, ToolkitPage, is_composio_https_url,
};
pub use curated::{Curated, GROUPS, Popular, group_of, short_name};
pub use error::{ComposioError, Result};
pub use keys::{KEY_SECRET, KEY_SETUP_URL, KeySource, LOGIN_URL, LocalKeySource, Stored};
pub use probe::ProbeOutcome;
pub use risk::{ActionMeta, Risk, classify};
pub use schema::{clean_text, sanitize, validate};
pub use service::{
    ActionInfo, ActionPreview, AppView, Audit, ComposioService, ConnState, Linkage, LoginStart,
    LoginState, PREVIEW_ACTIONS, PREVIEW_DESCRIPTION_CHARS, ServiceConfig,
};
pub use state::{AppConfig, FILE_NAME, LoginIds, StateFile};

#[cfg(test)]
mod tests;
