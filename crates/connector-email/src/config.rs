//! Account configuration. Passwords are never stored here, only a [`SecretRef`].

use std::time::Duration;

use secret_store::SecretRef;
use serde::{Deserialize, Serialize};

use crate::error::EmailError;

/// How the SMTP connection is secured. There is deliberately no plaintext variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// TLS from the first byte (usually port 465).
    #[default]
    Implicit,
    /// Plain greeting upgraded with `STARTTLS`, which is *required* (usually port 587).
    StartTls,
}

/// What kind of mailbox an account is. Old `email.toml` files have no `kind`
/// and load as [`AccountKind::ImapSmtp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccountKind {
    /// The user's own mailbox over IMAP and SMTP.
    #[default]
    #[serde(rename = "imap_smtp")]
    ImapSmtp,
    /// An `AgentMail` inbox reached over its REST API with the user's own key.
    #[serde(rename = "agentmail")]
    AgentMail,
}

impl AccountKind {
    /// Whether this kind is the default (not written to `email.toml`).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::ImapSmtp
    }

    /// The stable name used in the file and the API.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImapSmtp => "imap_smtp",
            Self::AgentMail => "agentmail",
        }
    }
}

/// IMAP server (always implicit TLS, usually port 993).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapSettings {
    /// Server host name.
    pub host: String,
    /// Port (default 993).
    #[serde(default = "default_imap_port")]
    pub port: u16,
}

/// SMTP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettings {
    /// Server host name.
    pub host: String,
    /// Port (default 465).
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// Transport security (default implicit TLS).
    #[serde(default)]
    pub security: SmtpSecurity,
}

impl Default for ImapSettings {
    /// "Not configured": what an API-backed account carries.
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_imap_port(),
        }
    }
}

impl Default for SmtpSettings {
    /// "Not configured": what an API-backed account carries.
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_smtp_port(),
            security: SmtpSecurity::default(),
        }
    }
}

impl ImapSettings {
    pub(crate) fn is_unset(&self) -> bool {
        self.host.is_empty()
    }
}

impl SmtpSettings {
    pub(crate) fn is_unset(&self) -> bool {
        self.host.is_empty()
    }
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    465
}
fn default_drafts() -> String {
    "Drafts".to_string()
}
fn default_timeout() -> u64 {
    30
}

/// One mailbox account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    /// Short name the agent uses to pick the account (e.g. `"work"`).
    pub name: String,
    /// The address mail is sent from.
    pub address: String,
    /// Login name (defaults to `address` when empty).
    #[serde(default)]
    pub username: String,
    /// Reference to the secret in the secret store: the (app) password of an
    /// IMAP/SMTP account, the API key of an `AgentMail` account.
    pub password: SecretRef,
    /// IMAP server (unused by API-backed accounts).
    #[serde(default, skip_serializing_if = "ImapSettings::is_unset")]
    pub imap: ImapSettings,
    /// SMTP server (unused by API-backed accounts).
    #[serde(default, skip_serializing_if = "SmtpSettings::is_unset")]
    pub smtp: SmtpSettings,
    /// Kind of mailbox; absent in older files = `imap_smtp`.
    #[serde(default, skip_serializing_if = "AccountKind::is_default")]
    pub kind: AccountKind,
    /// The provider's id of the inbox (`AgentMail` accounts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_id: Option<String>,
    /// Folder drafts are saved to.
    #[serde(default = "default_drafts")]
    pub drafts_folder: String,
    /// Extra trusted root certificate (PEM) for servers behind a private CA.
    #[serde(default)]
    pub extra_root_pem: Option<String>,
    /// Per-operation timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Bot (storage UUID, the id `ToolContext::bot_id` carries) that owns this
    /// account. A bound account is usable only by that bot; `None` keeps the
    /// original behaviour (any bot that may use the e-mail tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    /// Senders whose mail may later start a run (stored and validated now, only
    /// enforced by the inbound trigger). Entries are an address
    /// (`ann@example.com`) or a whole domain (`@example.com`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_senders: Vec<String>,
}

/// Longest bot id accepted (a UUID is 36 characters).
pub const MAX_BOT_ID_LEN: usize = 64;
/// Most entries `allowed_senders` may hold.
pub const MAX_ALLOWED_SENDERS: usize = 100;
/// Longest address or login name (an address is at most 254 characters).
pub const MAX_LOGIN_LEN: usize = 254;
/// Longest account name.
pub const MAX_ACCOUNT_NAME_LEN: usize = 64;

/// Check a server host name: ASCII letters, digits, `-`, `_` and `.` (or an
/// IPv6 literal), 1 to 253 bytes. No whitespace, control characters or URL
/// syntax, so a form value can never smuggle in anything but a host.
///
/// # Errors
/// [`EmailError::Config`] when the host is not acceptable.
pub fn validate_host(host: &str, what: &str) -> Result<(), EmailError> {
    if host.contains(':') {
        // Only an IPv6 literal may contain a colon (never `host:port`).
        return if host.parse::<std::net::Ipv6Addr>().is_ok() {
            Ok(())
        } else {
            Err(EmailError::Config(format!("invalid {what} host name")))
        };
    }
    let ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !host.starts_with(['.', '-'])
        && !host.contains("..");
    if ok {
        Ok(())
    } else {
        Err(EmailError::Config(format!("invalid {what} host name")))
    }
}

/// Check an account name used in URLs and secret names: 1 to 64 characters of
/// `[A-Za-z0-9._-]`.
///
/// # Errors
/// [`EmailError::Config`] when the name is not acceptable.
pub fn validate_account_name(name: &str) -> Result<(), EmailError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_ACCOUNT_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(EmailError::Config(
            "account name must be 1-64 characters of letters, digits, '.', '_' or '-'".into(),
        ))
    }
}

/// Trim and lower-case one `allowed_senders` entry and check it is an address
/// or an `@domain`.
///
/// # Errors
/// [`EmailError::Config`] for anything else.
pub fn normalize_sender(entry: &str) -> Result<String, EmailError> {
    let bad = || EmailError::Config("allowed senders must be addresses or @domains".into());
    let e = entry.trim().to_ascii_lowercase();
    if e.is_empty() || e.len() > 254 || e.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(bad());
    }
    if let Some(domain) = e.strip_prefix('@') {
        if validate_host(domain, "sender domain").is_ok() && domain.contains('.') {
            return Ok(e);
        }
        return Err(bad());
    }
    if e.parse::<email_address::EmailAddress>().is_ok() {
        Ok(e)
    } else {
        Err(bad())
    }
}

impl AccountConfig {
    /// Login name actually used.
    #[must_use]
    pub fn login(&self) -> &str {
        if self.username.is_empty() {
            &self.address
        } else {
            &self.username
        }
    }

    /// Operation timeout.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.clamp(1, 300))
    }

    /// Check the config is usable.
    ///
    /// # Errors
    /// [`EmailError::Config`] for empty names/hosts or an invalid sender address.
    pub fn validate(&self) -> Result<(), EmailError> {
        secret_store::validate_name(self.password.name())
            .map_err(|_| EmailError::Config("invalid password secret reference".into()))?;
        if self.login().len() > MAX_LOGIN_LEN || self.address.len() > MAX_LOGIN_LEN {
            return Err(EmailError::Config(
                "address or login name is too long".into(),
            ));
        }
        if self.login().chars().any(char::is_control) {
            return Err(EmailError::Config("invalid login name".into()));
        }
        if self.name.trim().is_empty() {
            return Err(EmailError::Config("account name is empty".into()));
        }
        match self.kind {
            AccountKind::ImapSmtp => self.validate_imap_smtp()?,
            AccountKind::AgentMail => self.validate_agentmail()?,
        }
        if let Some(bot) = &self.bot
            && (bot.is_empty()
                || bot.len() > MAX_BOT_ID_LEN
                || !bot.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(EmailError::Config("invalid bot id".into()));
        }
        if self.allowed_senders.len() > MAX_ALLOWED_SENDERS {
            return Err(EmailError::Config("too many allowed senders".into()));
        }
        for s in &self.allowed_senders {
            normalize_sender(s)?;
        }
        if self.address.parse::<email_address::EmailAddress>().is_err() {
            return Err(EmailError::Config(format!(
                "account {}: invalid sender address",
                self.name
            )));
        }
        Ok(())
    }

    fn validate_imap_smtp(&self) -> Result<(), EmailError> {
        crate::imap::check_folder(&self.drafts_folder)
            .map_err(|_| EmailError::Config("invalid drafts folder".into()))?;
        if self.imap.port == 0 || self.smtp.port == 0 {
            return Err(EmailError::Config("server port must be nonzero".into()));
        }
        if self.imap.host.trim().is_empty() || self.smtp.host.trim().is_empty() {
            return Err(EmailError::Config(format!(
                "account {}: imap/smtp host is empty",
                self.name
            )));
        }
        validate_host(&self.imap.host, "imap")?;
        validate_host(&self.smtp.host, "smtp")?;
        if self.inbox_id.is_some() {
            return Err(EmailError::Config(
                "an IMAP/SMTP account has no inbox id".into(),
            ));
        }
        Ok(())
    }

    fn validate_agentmail(&self) -> Result<(), EmailError> {
        let inbox = self.inbox_id.as_deref().unwrap_or_default();
        crate::agentmail::check_id(inbox, "inbox id")?;
        if !self.imap.is_unset() || !self.smtp.is_unset() {
            return Err(EmailError::Config(
                "an AgentMail account has no IMAP/SMTP servers".into(),
            ));
        }
        Ok(())
    }
}
