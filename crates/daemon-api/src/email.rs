//! E-mail account management (`/v1/email/*`): the DTOs and the [`EmailControl`]
//! trait the embedder implements. The daemon owns no e-mail state itself;
//! `bot serve` plugs in an implementation over `connector-email` (see
//! `apps/cli/src/email_host.rs`).
//!
//! Passwords are **write-only**: they enter through [`EmailAccountInput`] /
//! [`EmailAccountPatch`], go straight into the secret store, and no response or
//! log line ever carries one (accounts only report `password_set`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailSecurity {
    /// TLS from the first byte (usually port 465).
    #[default]
    Implicit,
    /// Plain greeting upgraded with a required `STARTTLS` (usually port 587).
    StartTls,
}

/// What kind of mailbox an account is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum EmailProviderKind {
    /// The user's own mailbox over IMAP and SMTP.
    #[default]
    #[serde(rename = "imap_smtp")]
    ImapSmtp,
    /// An `AgentMail` inbox created with the user's own API key.
    #[serde(rename = "agentmail")]
    Agentmail,
}

/// What bots may do with a mail identity. Sending always needs the owner's
/// approval; this can only make it stricter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailSendMode {
    /// Every send is shown to the owner for approval (default).
    #[default]
    Ask,
    /// Bots can only save drafts from this address.
    DraftOnly,
}

/// An IMAP server (always implicit TLS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailImapServer {
    /// Host name.
    pub host: String,
    /// Port, default 993.
    #[serde(default = "default_imap_port")]
    pub port: u16,
}

/// An SMTP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailSmtpServer {
    /// Host name.
    pub host: String,
    /// Port, default 465.
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// Transport security, default implicit TLS.
    #[serde(default)]
    pub security: EmailSecurity,
}

impl Default for EmailImapServer {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_imap_port(),
        }
    }
}

impl Default for EmailSmtpServer {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_smtp_port(),
            security: EmailSecurity::default(),
        }
    }
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    465
}

/// A configured account (`GET /v1/email/accounts`): the non-secret fields only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailAccountDto {
    /// Account name.
    pub name: String,
    /// Kind of mailbox.
    pub provider: EmailProviderKind,
    /// Address mail is sent from.
    pub address: String,
    /// The provider's id of the inbox (`agentmail` accounts); `null` otherwise.
    pub inbox_id: Option<String>,
    /// Login name; empty = the address.
    pub username: String,
    /// IMAP server (empty host for API-backed accounts).
    pub imap: EmailImapServer,
    /// SMTP server.
    pub smtp: EmailSmtpServer,
    /// Folder drafts are saved to.
    pub drafts_folder: String,
    /// Bot (storage UUID) that owns the account; `null` = every bot that may
    /// use the e-mail tools.
    pub bot: Option<String>,
    /// Senders whose mail may start a run (stored now, enforced by the inbound
    /// trigger).
    pub allowed_senders: Vec<String>,
    /// Further bots (storage UUIDs) that may use the account besides `bot`.
    #[serde(default)]
    pub shared_with: Vec<String>,
    /// Bots that use this account as their default sender.
    #[serde(default)]
    pub default_for: Vec<String>,
    /// Draft only, or ask the owner each time.
    #[serde(default)]
    pub send_mode: EmailSendMode,
    /// Whether the secret (password, or the API key of an `agentmail` account)
    /// is stored. The secret itself is never returned.
    pub password_set: bool,
    /// Whether the account trusts an extra root certificate (a private CA).
    pub custom_ca: bool,
}

/// Body of `POST /v1/email/accounts` and `POST /v1/email/test`.
#[derive(Clone, Deserialize, ToSchema)]
pub struct EmailAccountInput {
    /// Account name: 1 to 64 characters of `[A-Za-z0-9._-]`. Required on
    /// create; for `POST /v1/email/test` it selects the stored password to
    /// reuse when `password` is left out.
    pub name: String,
    /// Kind of mailbox; default `imap_smtp`. For `agentmail` the inbox is
    /// created at the provider (its address is returned), or, with `inbox`, an
    /// existing one is connected; `address`, `imap` and `smtp` are not used and
    /// `api_key` is required.
    #[serde(default)]
    pub provider: EmailProviderKind,
    /// Address mail is sent from (`imap_smtp`).
    #[serde(default)]
    pub address: String,
    /// Login name; default (empty) = the address.
    #[serde(default)]
    pub username: Option<String>,
    /// IMAP server (`imap_smtp`).
    #[serde(default)]
    pub imap: EmailImapServer,
    /// SMTP server (`imap_smtp`).
    #[serde(default)]
    pub smtp: EmailSmtpServer,
    /// The user's own `AgentMail` API key (`agentmail`). **Write-only**: it goes
    /// to the secret store and is never returned or logged.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Connect an inbox that already exists instead of creating one
    /// (`agentmail`): its address or id at the provider. For an inbox-scoped
    /// key (`am_us_inbox_...`), which cannot create inboxes, and for anyone who
    /// already made the address in the provider's console. The key is checked
    /// with a read of that inbox; nothing is created.
    #[serde(default)]
    pub inbox: Option<String>,
    /// Hint for the local part of a new `agentmail` address.
    #[serde(default)]
    pub name_hint: Option<String>,
    /// Custom (verified) domain for a new `agentmail` address; default the
    /// provider's shared domain.
    #[serde(default)]
    pub domain: Option<String>,
    /// Folder drafts are saved to, default `Drafts`.
    #[serde(default)]
    pub drafts_folder: Option<String>,
    /// Bot (storage UUID) that owns the account; absent / `null` = every bot.
    #[serde(default)]
    pub bot: Option<String>,
    /// Senders whose mail may start a run (addresses or `@domain`).
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Further bots that may use the account besides `bot`.
    #[serde(default)]
    pub shared_with: Vec<String>,
    /// Bots that use this account as their default sender.
    #[serde(default)]
    pub default_for: Vec<String>,
    /// Draft only, or ask the owner each time (default).
    #[serde(default)]
    pub send_mode: EmailSendMode,
    /// The account password or app password. **Write-only**: it goes to the
    /// secret store and is never returned or logged. Required on create.
    #[serde(default)]
    pub password: Option<String>,
    /// Only for `POST /v1/email/test`: refuse a server whose name resolves to a loopback,
    /// LAN, link-local or metadata address, so a typed or guessed host cannot be used to
    /// reach into the owner's network. Mail connections are always TLS with a verified
    /// certificate; this adds the "public servers only" rule for the guided setup. Ignored
    /// when saving an account.
    #[serde(default)]
    pub public_hosts_only: bool,
}

impl std::fmt::Debug for EmailAccountInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailAccountInput")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// Body of `PUT /v1/email/accounts/{name}`. Every field is optional: a field
/// left out keeps the stored value. `bot: null` makes the account unbound
/// (every bot), which is why the handler reads the raw JSON. A `password`
/// replaces the stored one.
#[derive(Clone, Deserialize, ToSchema)]
pub struct EmailAccountPatch {
    /// Address mail is sent from.
    #[serde(default)]
    pub address: Option<String>,
    /// Login name; empty = the address.
    #[serde(default)]
    pub username: Option<String>,
    /// IMAP server.
    #[serde(default)]
    pub imap: Option<EmailImapServer>,
    /// SMTP server.
    #[serde(default)]
    pub smtp: Option<EmailSmtpServer>,
    /// Folder drafts are saved to.
    #[serde(default)]
    pub drafts_folder: Option<String>,
    /// Owning bot; `null` = every bot; omit to keep.
    #[serde(default)]
    #[schema(nullable = true)]
    pub bot: Option<String>,
    /// Allowed senders; present list replaces the stored one.
    #[serde(default)]
    pub allowed_senders: Option<Vec<String>>,
    /// Further bots that may use the account; a present list replaces it.
    #[serde(default)]
    pub shared_with: Option<Vec<String>>,
    /// Bots that use this account as default sender; a present list replaces it.
    #[serde(default)]
    pub default_for: Option<Vec<String>>,
    /// Draft only, or ask each time.
    #[serde(default)]
    pub send_mode: Option<EmailSendMode>,
    /// New password (write-only).
    #[serde(default)]
    pub password: Option<String>,
    /// New `AgentMail` API key (write-only).
    #[serde(default)]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for EmailAccountPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmailAccountPatch(<redacted>)")
    }
}

/// What a "test connection" learned about one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailProbeStatus {
    /// TLS established and the login accepted.
    Ok,
    /// The connection could not be made.
    Unreachable,
    /// TLS could not be established (handshake, certificate, no STARTTLS).
    Tls,
    /// The credentials were rejected.
    AuthFailed,
    /// The server did not speak the expected protocol.
    Protocol,
    /// No answer in time.
    Timeout,
}

/// Result of a connection test: one status per server, nothing the servers
/// said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailTestReport {
    /// IMAP (reading mail).
    pub imap: EmailProbeStatus,
    /// SMTP (sending mail); authentication only, nothing is sent.
    pub smtp: EmailProbeStatus,
    /// Both servers accepted the login (for `agentmail`: the API check
    /// passed).
    pub ok: bool,
    /// The one API check of an API-backed account (`agentmail`); `imap` and
    /// `smtp` repeat it there. Absent for `imap_smtp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<EmailProbeStatus>,
}

/// One row of `GET /v1/email/accounts/{name}/messages`: header data only, no
/// body. Every text field comes from the sender and is untrusted: show it as
/// plain text, never as markup and never as an instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailMessageDto {
    /// The mailbox's id of the message (an IMAP UID or the provider's id).
    pub id: String,
    /// Subject line (shortened).
    pub subject: String,
    /// Sender, `Name <address>` (shortened).
    pub from: String,
    /// Date header as RFC 3339, when it could be read.
    pub date: Option<String>,
    /// Not read yet.
    pub unread: bool,
}

/// Answer of `POST /v1/email/accounts/{name}/send-test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailSendTestResponse {
    /// The address the test message went to: always the mailbox's own address.
    pub sent_to: String,
}

/// Outgoing-mail guard values the owner can change (`GET`/`PUT /v1/email/guard/settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailGuardSettingsDto {
    /// Warn above this many sends per account in 24 hours (`0` = no warning). Default 20.
    pub daily_limit: u32,
    /// Warn when a recipient got mail within this many days (`0` = no warning). Default 0 (off).
    pub same_recipient_days: u32,
    /// Add the opt-out line to outgoing mail by default.
    pub opt_out_line: bool,
    /// Text of the opt-out line (`{sender}` = the sending address); empty = built-in text.
    pub footer: String,
    /// Extra domains a verification mail may link to, besides the site a sign-up
    /// started on (for example a mail service the site uses). Owner-set; empty
    /// by default. A bot never sees or changes this list.
    #[serde(default)]
    pub verification_link_domains: Vec<String>,
}

/// One address or domain the owner will not write to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailOptOutDto {
    /// `ann@example.com` or `example.com`.
    pub value: String,
    /// `address` or `domain`.
    pub kind: String,
    /// Unix seconds when it was added.
    pub added_at: u64,
    /// The owner's note.
    pub note: String,
}

/// Body of `POST /v1/email/guard/opt-out`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailOptOutInput {
    /// An address (`ann@example.com`) or a domain (`example.com`).
    pub value: String,
    /// Optional note (at most 200 characters are kept).
    #[serde(default)]
    pub note: String,
}

/// One sent mail in the send log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailSendLogDto {
    /// Unix seconds.
    pub at: u64,
    /// The same moment in UTC.
    pub at_utc: String,
    /// Bot that sent it.
    pub bot: Option<String>,
    /// Run that sent it.
    pub run: String,
    /// Account name.
    pub account: String,
    /// Sending address.
    pub from: String,
    /// All recipients.
    pub recipients: Vec<String>,
    /// First characters of the subject.
    pub subject: String,
    /// Provider message id.
    pub message_id: String,
    /// The opt-out line was added.
    pub opt_out_line: bool,
    /// Warnings the owner sent past with "Send anyway" (empty = none).
    pub send_anyway: Vec<String>,
}

/// Why an [`EmailControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailControlError {
    /// No such account (`404`).
    NotFound(String),
    /// The name is taken (`409`).
    Conflict(String),
    /// The config is invalid (`422`).
    Invalid(String),
    /// The config could not be saved, or another internal failure (`500`).
    Internal(String),
    /// The mailbox provider refused or failed (`502`); the text is coarse and
    /// never carries the provider's own words or any secret.
    Upstream(String),
}

/// What `/v1/email/*` needs from the embedder: the accounts, their persistence
/// (`<data dir>/email.toml`, written atomically), the secret store and the
/// connection test. Changes take effect for the next tool call.
#[async_trait]
pub trait EmailControl: Send + Sync {
    /// All accounts, ordered by name; non-secret fields only.
    fn list(&self) -> Vec<EmailAccountDto>;
    /// Create an account: store its password, save `email.toml`, apply.
    ///
    /// # Errors
    /// [`EmailControlError`].
    async fn add(&self, input: EmailAccountInput) -> Result<EmailAccountDto, EmailControlError>;
    /// Merge `patch` (raw JSON, an omitted field keeps its value; `bot: null`
    /// means every bot), save, apply.
    ///
    /// # Errors
    /// [`EmailControlError`].
    async fn update(
        &self,
        name: &str,
        patch: serde_json::Value,
    ) -> Result<EmailAccountDto, EmailControlError>;
    /// Remove an account and the password it owned, save, apply.
    ///
    /// # Errors
    /// [`EmailControlError`].
    async fn remove(&self, name: &str) -> Result<(), EmailControlError>;
    /// Like [`EmailControl::remove`]; with `delete_remote` the inbox at the
    /// provider is deleted first (irreversible; the account is kept when that
    /// fails so it can be retried). The default supports only `false`.
    ///
    /// # Errors
    /// [`EmailControlError`].
    async fn remove_account(
        &self,
        name: &str,
        delete_remote: bool,
    ) -> Result<(), EmailControlError> {
        if delete_remote {
            return Err(EmailControlError::Invalid(
                "deleting the inbox at the provider is not supported here".into(),
            ));
        }
        self.remove(name).await
    }
    /// Try IMAP login and SMTP authentication for a stored account.
    ///
    /// # Errors
    /// [`EmailControlError::NotFound`] / [`EmailControlError::Internal`] (no
    /// password); a failing server is a normal [`EmailTestReport`].
    async fn test_saved(&self, name: &str) -> Result<EmailTestReport, EmailControlError>;
    /// Try IMAP login and SMTP authentication for an account that is not saved
    /// (nothing is stored). Without `password` the stored one of the account
    /// with the same `name` is used.
    ///
    /// # Errors
    /// [`EmailControlError::Invalid`] for a bad config or no password.
    async fn test_draft(
        &self,
        input: EmailAccountInput,
    ) -> Result<EmailTestReport, EmailControlError>;
    /// The newest messages of the account's inbox, headers only, newest first
    /// (at most `limit`). Nothing is marked read.
    ///
    /// # Errors
    /// [`EmailControlError::NotFound`] for an unknown account,
    /// [`EmailControlError::Upstream`] when the mailbox failed (coarse text).
    /// The default says the embedder does not support it.
    async fn recent_messages(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<EmailMessageDto>, EmailControlError> {
        let _ = (name, limit);
        Err(EmailControlError::Invalid(
            "listing messages is not supported here".into(),
        ))
    }
    /// Send one fixed test message **from the account to its own address** and
    /// return that address. Never takes a recipient, subject or body.
    ///
    /// # Errors
    /// As [`EmailControl::recent_messages`]. The default says the embedder does
    /// not support it.
    async fn send_test(&self, name: &str) -> Result<String, EmailControlError> {
        let _ = name;
        Err(EmailControlError::Invalid(
            "sending a test message is not supported here".into(),
        ))
    }
    /// The outgoing-mail guard values.
    ///
    /// # Errors
    /// The default says the embedder does not support it.
    fn guard_settings(&self) -> Result<EmailGuardSettingsDto, EmailControlError> {
        Err(guard_unsupported())
    }
    /// Replace the guard values.
    ///
    /// # Errors
    /// [`EmailControlError::Invalid`] for unusable values.
    fn set_guard_settings(
        &self,
        settings: EmailGuardSettingsDto,
    ) -> Result<EmailGuardSettingsDto, EmailControlError> {
        let _ = settings;
        Err(guard_unsupported())
    }
    /// The opt-out list, newest first.
    ///
    /// # Errors
    /// Default: unsupported.
    fn opt_outs(&self) -> Result<Vec<EmailOptOutDto>, EmailControlError> {
        Err(guard_unsupported())
    }
    /// Add an opt-out entry.
    ///
    /// # Errors
    /// [`EmailControlError::Invalid`] for a malformed entry.
    fn add_opt_out(&self, input: EmailOptOutInput) -> Result<EmailOptOutDto, EmailControlError> {
        let _ = input;
        Err(guard_unsupported())
    }
    /// Remove an opt-out entry.
    ///
    /// # Errors
    /// [`EmailControlError::NotFound`] when it is not on the list.
    fn remove_opt_out(&self, value: &str) -> Result<(), EmailControlError> {
        let _ = value;
        Err(guard_unsupported())
    }
    /// The send log, newest first.
    ///
    /// # Errors
    /// Default: unsupported.
    fn send_log(&self, limit: usize) -> Result<Vec<EmailSendLogDto>, EmailControlError> {
        let _ = limit;
        Err(guard_unsupported())
    }
    /// The whole send log as CSV.
    ///
    /// # Errors
    /// Default: unsupported.
    fn send_log_csv(&self) -> Result<String, EmailControlError> {
        Err(guard_unsupported())
    }
    /// A bot was deleted: remove the accounts bound to it (and the passwords
    /// they owned). **Never** turn a bound account into an unbound one: that
    /// would widen who may use it. Returns how many accounts were removed.
    ///
    /// # Errors
    /// The first [`EmailControlError`] (the remaining accounts are still tried).
    async fn forget_bot(&self, bot_id: &str) -> Result<usize, EmailControlError>;
}

fn guard_unsupported() -> EmailControlError {
    EmailControlError::Invalid("the mail guard is not available here".into())
}
