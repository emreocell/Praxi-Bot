//! E-mail connector: IMAP read and SMTP send as agent tools.
//!
//! - Reading: [`tools::LIST_FOLDERS`], [`tools::SEARCH`], [`tools::READ`]
//!   (`Effect::Read`), [`tools::MARK_READ`] (`Effect::Write`).
//! - Writing: [`tools::DRAFT`] saves to the drafts folder (`Effect::Write`);
//!   [`tools::SEND`] sends immediately and is `Effect::Destructive`, so the
//!   permission layer always asks for approval.
//! - TLS is mandatory (implicit TLS for IMAP, implicit TLS or required
//!   `STARTTLS` for SMTP) and certificates are always verified.
//! - Credentials are [`secret_store::SecretRef`]s resolved only when connecting.
//!
//! Entry points: [`tools`] / [`register`]. Wiring into `builtin-tools` is a
//! follow-up. Limitation: folder names are passed as-is (no modified UTF-7
//! conversion), so non-ASCII folder names must be given in the server's
//! encoding.

pub mod config;
pub mod connector;
pub mod error;
pub mod imap;
pub mod message;
pub mod smtp;
mod tls;
pub mod tools;

use std::sync::Arc;

use tool_registry::{Tool, ToolRegistry};

pub use config::{AccountConfig, ImapSettings, SmtpSecurity, SmtpSettings};
pub use connector::{EmailConnector, SearchResult};
pub use error::EmailError;
pub use imap::{FolderInfo, SearchQuery};
pub use message::{AttachmentInfo, MessageDetail, MessageSummary};
pub use smtp::OutgoingMail;
pub use tools::TOOL_NAMES;

/// Build the e-mail tools without registering them.
#[must_use]
pub fn tools(connector: &Arc<EmailConnector>) -> Vec<Arc<dyn Tool>> {
    tools::tools(connector)
}

/// Register the e-mail tools in `registry`; returns how many were added.
#[must_use]
pub fn register(registry: &ToolRegistry, connector: &Arc<EmailConnector>) -> usize {
    tools(connector)
        .into_iter()
        .filter(|t| registry.register(t.clone()))
        .count()
}

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests;
