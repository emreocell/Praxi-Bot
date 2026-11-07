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
    /// Reference to the (app) password in the secret store.
    pub password: SecretRef,
    /// IMAP server.
    pub imap: ImapSettings,
    /// SMTP server.
    pub smtp: SmtpSettings,
    /// Folder drafts are saved to.
    #[serde(default = "default_drafts")]
    pub drafts_folder: String,
    /// Extra trusted root certificate (PEM) for servers behind a private CA.
    #[serde(default)]
    pub extra_root_pem: Option<String>,
    /// Per-operation timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
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
        crate::imap::check_folder(&self.drafts_folder)
            .map_err(|_| EmailError::Config("invalid drafts folder".into()))?;
        secret_store::validate_name(self.password.name())
            .map_err(|_| EmailError::Config("invalid password secret reference".into()))?;
        if self.imap.port == 0 || self.smtp.port == 0 {
            return Err(EmailError::Config("server port must be nonzero".into()));
        }
        if self.login().contains(['\r', '\n', '\0']) {
            return Err(EmailError::Config("invalid login name".into()));
        }
        if self.name.trim().is_empty() {
            return Err(EmailError::Config("account name is empty".into()));
        }
        if self.imap.host.trim().is_empty() || self.smtp.host.trim().is_empty() {
            return Err(EmailError::Config(format!(
                "account {}: imap/smtp host is empty",
                self.name
            )));
        }
        if self.address.parse::<email_address::EmailAddress>().is_err() {
            return Err(EmailError::Config(format!(
                "account {}: invalid sender address",
                self.name
            )));
        }
        Ok(())
    }
}
