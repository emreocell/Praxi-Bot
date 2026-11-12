//! The mailbox abstraction: [`MailboxProvider`].
//!
//! A provider is one way of reaching a mailbox: the user's own server over
//! IMAP/SMTP ([`crate::imap_provider::ImapSmtpProvider`]) or an API such as
//! `AgentMail` ([`crate::agentmail::AgentMailProvider`]). Nothing here assumes
//! IMAP: message ids are opaque strings, folders are optional, push is not
//! required (`list_new` is cursor based) and every operation a provider lacks
//! answers [`EmailError::Unsupported`], which reaches the model as a clear
//! "not supported by this mailbox".
//!
//! A provider value is bound to credentials and (for API providers) to one
//! inbox. Operations that need an inbox when none is bound fail with
//! [`EmailError::Input`].

use async_trait::async_trait;
use serde::Serialize;

use crate::config::AccountKind;
use crate::error::EmailError;
use crate::imap::{FolderInfo, SearchQuery};
use crate::message::{MessageDetail, MessageSummary};
use crate::smtp::OutgoingMail;

/// What a provider can do. Callers check these instead of assuming IMAP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// Can create inboxes with the user's key ([`MailboxProvider::create_inbox`]).
    pub create_inbox: bool,
    /// Can delete the bound inbox remotely ([`MailboxProvider::delete_inbox`]).
    pub delete_inbox: bool,
    /// Has folders ([`MailboxProvider::list_folders`], folder arguments count).
    pub folders: bool,
    /// Supports [`MailboxProvider::search`].
    pub search: bool,
    /// Supports [`MailboxProvider::mark_read`].
    pub mark_read: bool,
    /// Supports [`MailboxProvider::save_draft`].
    pub drafts: bool,
    /// Supports [`MailboxProvider::list_new`] (cursor polling).
    pub list_new: bool,
    /// Supports [`MailboxProvider::get_attachment`].
    pub attachments: bool,
    /// Supports [`MailboxProvider::reply`].
    pub reply: bool,
    /// The provider can push new mail (WebSocket, IDLE); not used before 190c.
    pub push: bool,
}

/// What `create_inbox` needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateInbox {
    /// Hint for the local part of the address (sanitised by the provider).
    pub name_hint: Option<String>,
    /// Custom domain; `None` = the provider's shared domain.
    pub domain: Option<String>,
    /// Idempotency key: the same key never creates a second inbox. Must not
    /// contain `@`.
    pub client_id: String,
}

/// A created (or re-found) inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxInfo {
    /// The provider's id of the inbox.
    pub inbox_id: String,
    /// The e-mail address of the inbox.
    pub address: String,
}

/// New messages since a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessages {
    /// Oldest first.
    pub messages: Vec<MessageSummary>,
    /// Pass this to the next `list_new` call. Delivery is at-least-once: a
    /// message may be listed again, so callers de-duplicate on the message id.
    pub next_cursor: Option<String>,
}

/// An attachment's bytes, already size-checked.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentData {
    /// Raw content. Never executed; callers only store it.
    pub bytes: Vec<u8>,
    /// Declared MIME type, if any.
    pub content_type: Option<String>,
    /// Declared (untrusted) file name, if any.
    pub filename: Option<String>,
}

impl std::fmt::Debug for AttachmentData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachmentData")
            .field("len", &self.bytes.len())
            .field("content_type", &self.content_type)
            .finish_non_exhaustive()
    }
}

/// What a send or reply returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Provider message id (API providers) or the server's reply line (SMTP).
    pub id: String,
    /// Thread id when the provider has threads.
    pub thread_id: Option<String>,
}

/// Search arguments.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    /// Folder (providers without folders ignore it).
    pub folder: &'a str,
    /// Criteria.
    pub query: &'a SearchQuery,
    /// Page size.
    pub limit: usize,
}

/// Result of a provider search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Newest first.
    pub messages: Vec<MessageSummary>,
    /// Total matches when the provider knows it, else the returned count.
    pub total: usize,
}

fn unsupported<T>(what: &str) -> Result<T, EmailError> {
    Err(EmailError::unsupported(what))
}

/// One way of reaching a mailbox. All optional methods have an "unsupported"
/// default so a provider implements only what it offers and advertises it in
/// [`MailboxProvider::capabilities`].
#[async_trait]
pub trait MailboxProvider: Send + Sync {
    /// Which kind of account this serves.
    fn kind(&self) -> AccountKind;

    /// What it can do.
    fn capabilities(&self) -> Capabilities;

    /// Create an inbox with the user's own key. Idempotent in `client_id`.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default; provider failures otherwise.
    async fn create_inbox(&self, _req: &CreateInbox) -> Result<InboxInfo, EmailError> {
        unsupported("creating a mailbox")
    }

    /// Delete the bound inbox at the provider (irreversible).
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn delete_inbox(&self) -> Result<(), EmailError> {
        unsupported("deleting a mailbox")
    }

    /// An authenticated, cheap call that proves the credentials work. Nothing
    /// is read or sent.
    ///
    /// # Errors
    /// Provider failures.
    async fn check(&self) -> Result<(), EmailError>;

    /// Messages that arrived after `cursor` (`None` = the most recent ones).
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn list_new(
        &self,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<NewMessages, EmailError> {
        unsupported("listing new messages")
    }

    /// Folders of the mailbox.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn list_folders(&self) -> Result<Vec<FolderInfo>, EmailError> {
        unsupported("folders")
    }

    /// Search, newest first.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn search(&self, _req: &SearchRequest<'_>) -> Result<Found, EmailError> {
        unsupported("searching")
    }

    /// Fetch one message (does not mark it read). `id` is the provider's
    /// message id (for IMAP the decimal UID). Bodies are untrusted content.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn get_message(
        &self,
        _folder: &str,
        _id: &str,
        _max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        unsupported("reading a message")
    }

    /// Mark a message read or unread.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn mark_read(&self, _folder: &str, _id: &str, _read: bool) -> Result<(), EmailError> {
        unsupported("marking messages read")
    }

    /// Save a draft; nothing is sent. Returns where it was saved.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn save_draft(&self, _mail: &OutgoingMail) -> Result<String, EmailError> {
        unsupported("saving drafts")
    }

    /// Download an attachment, refusing more than `max_bytes`.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn get_attachment(
        &self,
        _message_id: &str,
        _attachment_id: &str,
        _max_bytes: usize,
    ) -> Result<AttachmentData, EmailError> {
        unsupported("downloading attachments")
    }

    /// Send a message **now**. Callers must have obtained approval.
    ///
    /// # Errors
    /// Provider failures.
    async fn send(&self, mail: &OutgoingMail) -> Result<Receipt, EmailError>;

    /// Reply to a message in its thread.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] by default.
    async fn reply(
        &self,
        _message_id: &str,
        _mail: &OutgoingMail,
        _reply_all: bool,
    ) -> Result<Receipt, EmailError> {
        unsupported("replying in a thread")
    }
}
