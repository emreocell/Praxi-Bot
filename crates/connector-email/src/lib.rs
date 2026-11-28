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
//! follow-up. IMAP folder selectors accept Unicode or a complete modified
//! UTF-7 wire name. LIST names are decoded and capped for display and may not
//! be exact selectors: if a display name looks like modified UTF-7, pass the
//! original wire name to disambiguate.

mod address;
pub mod agentmail;
pub mod attachment;
pub mod channel;
pub mod config;
pub mod connector;
mod cred;
pub mod error;
pub mod guard;
pub mod imap;
pub mod imap_provider;
pub mod message;
pub mod probe;
pub mod provider;
pub mod reply_classification;
pub mod smtp;
mod tls;
pub mod tools;
pub mod verify;
pub mod wait;
pub mod wait_store;

use std::sync::Arc;

use tool_registry::{Tool, ToolRegistry};

pub use agentmail::{AgentMailClient, AgentMailEndpoint, AgentMailProvider, ApiKey};
pub use attachment::AttachmentPlace;
pub use config::{
    AccountConfig, AccountKind, IdentitySettings, ImapSettings, SendMode, SmtpSecurity,
    SmtpSettings, normalize_sender, validate_account_name, validate_host,
};
pub use connector::{
    ATTACHMENTS_DIR_NAME, EmailConnector, MAX_ATTACHMENT_BYTES, SavedAttachment, Scoped,
    SearchResult,
};
pub use error::{EmailError, ProviderError};
pub use guard::{GuardReport, GuardSettings, MailGuard, OptOut, SendLogEntry, Warning};
pub use imap::{FolderInfo, SearchQuery};
pub use imap_provider::ImapSmtpProvider;
pub use message::{AttachmentInfo, MessageDetail, MessageSummary};
pub use probe::{ProbeReport, ProbeStatus, probe};
pub use provider::{
    AttachmentData, Capabilities, CreateInbox, Found, InboxInfo, MailboxProvider, NewMessages,
    Receipt, SearchRequest,
};
pub use smtp::{CalendarPart, OutgoingMail};
pub use tools::TOOL_NAMES;
pub use wait::{
    JunkCandidate, LinkBasis, MailHandle, PageOrigin, PageOriginSlot, TrackingResolver,
    WaitBinding, WaitEnd, WaitFilter, WaitOwner, WaitPlan, WaitProgress, Waiters, WebResolver,
    expand_tracking_links, found_json, junk_folders, link_basis, timed_out_json, wait_for_mail,
};
pub use wait_store::WaitStore;

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
mod completeness_tests;
#[cfg(test)]
mod fake_imap;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod junk_tests;
#[cfg(test)]
mod kind_tests;
#[cfg(test)]
mod mailbox_tests;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod wait_tests;
