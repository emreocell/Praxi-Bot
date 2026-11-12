//! "Test connection": TLS connect, IMAP login and SMTP authentication, nothing
//! else. No mail is sent or read.
//!
//! The report is deliberately coarse: one status per server and nothing that
//! came off the wire (no banner, no server text, no host echo), so it cannot
//! echo credentials a server reflects and tells a caller little beyond "this
//! is a reachable, authenticating IMAP/SMTP server or it is not".

use std::time::Duration;

use mail_send::SmtpClientBuilder;
use serde::Serialize;

use crate::config::{AccountConfig, SmtpSecurity};
use crate::imap::{ConnectStage, ImapSession};

/// Upper bound for the whole probe, whatever the account's own timeout says.
pub const MAX_PROBE_SECS: u64 = 15;

/// Outcome for one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    /// TLS established and the login was accepted.
    Ok,
    /// The TCP connection failed.
    Unreachable,
    /// TLS could not be established (handshake, certificate, no STARTTLS).
    Tls,
    /// The server rejected the credentials.
    AuthFailed,
    /// The server answered something that is not the expected protocol.
    Protocol,
    /// No answer in time.
    Timeout,
}

/// Result of [`probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProbeReport {
    /// IMAP (reading mail).
    pub imap: ProbeStatus,
    /// SMTP (sending mail); authentication only, nothing is sent.
    pub smtp: ProbeStatus,
}

impl ProbeReport {
    /// Both servers accepted the login.
    #[must_use]
    pub fn all_ok(&self) -> bool {
        self.imap == ProbeStatus::Ok && self.smtp == ProbeStatus::Ok
    }
}

/// Try IMAP login and SMTP authentication for `acct` with `password`. TLS is
/// used exactly as configured (certificates always verified); the connection is
/// closed again without issuing any mail command.
pub async fn probe(acct: &AccountConfig, password: &str) -> ProbeReport {
    let limit = acct.timeout().min(Duration::from_secs(MAX_PROBE_SECS));
    let (imap, smtp) = tokio::join!(
        async {
            tokio::time::timeout(limit, probe_imap(acct, password))
                .await
                .unwrap_or(ProbeStatus::Timeout)
        },
        async {
            tokio::time::timeout(
                limit + Duration::from_secs(1),
                probe_smtp(acct, password, limit),
            )
            .await
            .unwrap_or(ProbeStatus::Timeout)
        }
    );
    ProbeReport { imap, smtp }
}

async fn probe_imap(acct: &AccountConfig, password: &str) -> ProbeStatus {
    match ImapSession::connect_staged(acct, password).await {
        Ok(session) => {
            session.logout().await;
            ProbeStatus::Ok
        }
        Err(ConnectStage::Connect) => ProbeStatus::Unreachable,
        Err(ConnectStage::Tls) => ProbeStatus::Tls,
        Err(ConnectStage::Greeting) => ProbeStatus::Protocol,
        Err(ConnectStage::Login) => ProbeStatus::AuthFailed,
    }
}

async fn probe_smtp(acct: &AccountConfig, password: &str, limit: Duration) -> ProbeStatus {
    let Ok(connector) = crate::tls::connector(acct.extra_root_pem.as_deref()) else {
        return ProbeStatus::Tls;
    };
    let mut builder = SmtpClientBuilder::new(acct.smtp.host.clone(), acct.smtp.port)
        .implicit_tls(acct.smtp.security == SmtpSecurity::Implicit)
        .credentials((acct.login().to_owned(), password.to_owned()))
        .timeout(limit);
    builder.tls_connector = connector;
    // `connect` performs EHLO and AUTH when credentials are set; nothing more.
    match builder.connect().await {
        Ok(client) => {
            let _ = client.quit().await;
            ProbeStatus::Ok
        }
        Err(e) => match e {
            // A failed TLS handshake surfaces as an `InvalidData` I/O error.
            mail_send::Error::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                ProbeStatus::Tls
            }
            mail_send::Error::Io(_) => ProbeStatus::Unreachable,
            mail_send::Error::Tls(_) | mail_send::Error::InvalidTLSName => ProbeStatus::Tls,
            mail_send::Error::AuthenticationFailed(_)
            | mail_send::Error::Auth(_)
            | mail_send::Error::MissingCredentials => ProbeStatus::AuthFailed,
            mail_send::Error::Timeout => ProbeStatus::Timeout,
            _ => ProbeStatus::Protocol,
        },
    }
}
