//! E-mail connector: IMAP read and SMTP send as agent tools.
//!
//! - Reading: [`tools::LIST_FOLDERS`], [`tools::SEARCH`], [`tools::READ`]
//!   (`Effect::Read`), [`tools::MARK_READ`] (`Effect::Write`).
//! - Writing: [`tools::DRAFT`] saves to the drafts folder (`Effect::Write`);
//!   [`tools::SEND`] sends immediately and is `Effect::Destructive`, so the
//!   caller must route it through the platform's approval gate before calling.
//! - TLS is mandatory (implicit TLS for IMAP, implicit TLS or required
//!   `STARTTLS` for SMTP) and certificates are always verified.
//! - Credentials are [`secret_store::SecretRef`]s resolved only when connecting.
//!
//! Entry points: [`tools`] / [`register`]. Wiring into `builtin-tools` is a
//! follow-up. Limitation: folder names are passed as-is (no modified UTF-7
//! conversion), so non-ASCII folder names must be given in the server's
//! encoding.

pub mod agentmail;
pub mod config;
pub mod connector;
mod cred;
pub mod error;
pub mod imap;
pub mod imap_provider;
pub mod message;
pub mod probe;
pub mod provider;
pub mod smtp;
mod tls;
pub mod tools;

use std::sync::Arc;

use tool_registry::{Tool, ToolRegistry};

pub use agentmail::{AgentMailClient, AgentMailEndpoint, AgentMailProvider, ApiKey};
pub use config::{
    AccountConfig, AccountKind, ImapSettings, SmtpSecurity, SmtpSettings, normalize_sender,
    validate_account_name, validate_host,
};
pub use connector::{EmailConnector, MAX_ATTACHMENT_BYTES, Scoped, SearchResult};
pub use error::{EmailError, ProviderError};
pub use imap::{FolderInfo, SearchQuery};
pub use imap_provider::ImapSmtpProvider;
pub use message::{AttachmentInfo, MessageDetail, MessageSummary};
pub use probe::{ProbeReport, ProbeStatus, probe};
pub use provider::{
    AttachmentData, Capabilities, CreateInbox, Found, InboxInfo, MailboxProvider, NewMessages,
    Receipt, SearchRequest,
};
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
mod bot_tests;
#[cfg(test)]
mod kind_tests;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;
