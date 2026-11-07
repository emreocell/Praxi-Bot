//! Message composition and SMTP submission (TLS required) built on `lettre`.

use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::Deserialize;

use crate::config::{AccountConfig, SmtpSecurity};
use crate::error::EmailError;

/// Maximum recipients (to + cc + bcc) in one message.
pub const MAX_RECIPIENTS: usize = 50;
/// Maximum body size in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// A message to compose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutgoingMail {
    /// Primary recipients.
    #[serde(default)]
    pub to: Vec<String>,
    /// Carbon copy.
    #[serde(default)]
    pub cc: Vec<String>,
    /// Blind carbon copy.
    #[serde(default)]
    pub bcc: Vec<String>,
    /// Subject line.
    #[serde(default)]
    pub subject: String,
    /// Plain-text body.
    #[serde(default)]
    pub body: String,
}

fn mailbox(s: &str) -> Result<Mailbox, EmailError> {
    s.trim()
        .parse::<Mailbox>()
        .map_err(|e| EmailError::Input(format!("invalid address {s:?}: {e}")))
}

/// Compose an RFC 5322 message.
///
/// # Errors
/// [`EmailError::Input`] for invalid or missing recipients, oversized bodies, or
/// line breaks in the subject.
pub fn compose(from: &str, mail: &OutgoingMail) -> Result<Message, EmailError> {
    let total = mail.to.len() + mail.cc.len() + mail.bcc.len();
    if total == 0 {
        return Err(EmailError::Input("at least one recipient is required".into()));
    }
    if total > MAX_RECIPIENTS {
        return Err(EmailError::Input(format!(
            "too many recipients ({total} > {MAX_RECIPIENTS})"
        )));
    }
    if mail.body.len() > MAX_BODY_BYTES {
        return Err(EmailError::Input("body is too large".into()));
    }
    if mail.subject.contains(['\r', '\n']) {
        return Err(EmailError::Input("subject must be a single line".into()));
    }
    let mut b = Message::builder().from(mailbox(from)?).subject(&mail.subject);
    for a in &mail.to {
        b = b.to(mailbox(a)?);
    }
    for a in &mail.cc {
        b = b.cc(mailbox(a)?);
    }
    for a in &mail.bcc {
        b = b.bcc(mailbox(a)?);
    }
    b.header(ContentType::TEXT_PLAIN)
        .body(mail.body.clone())
        .map_err(|e| EmailError::Input(format!("cannot build message: {e}")))
}

fn smtp_err(e: impl std::fmt::Display) -> EmailError {
    EmailError::Smtp(e.to_string())
}

/// Submit a composed message. TLS is always required; there is no plaintext path.
///
/// # Errors
/// TLS, authentication or delivery failures.
pub async fn send(acct: &AccountConfig, password: &str, msg: Message) -> Result<String, EmailError> {
    let host = acct.smtp.host.clone();
    let mut tls = TlsParameters::builder(host.clone());
    if let Some(pem) = &acct.extra_root_pem {
        let cert = Certificate::from_pem(pem.as_bytes())
            .map_err(|e| EmailError::Tls(format!("bad root certificate: {e}")))?;
        tls = tls.add_root_certificate(cert);
    }
    let params = tls
        .build_rustls()
        .map_err(|e| EmailError::Tls(e.to_string()))?;
    let mode = match acct.smtp.security {
        SmtpSecurity::Implicit => Tls::Wrapper(params),
        SmtpSecurity::StartTls => Tls::Required(params),
    };
    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(acct.smtp.port)
        .tls(mode)
        .credentials(Credentials::new(
            acct.login().to_string(),
            password.to_string(),
        ))
        .timeout(Some(acct.timeout()))
        .build();
    let resp = transport.send(msg).await.map_err(smtp_err)?;
    Ok(format!(
        "{} {}",
        resp.code(),
        resp.message().collect::<Vec<_>>().join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail() -> OutgoingMail {
        OutgoingMail {
            to: vec!["Bob <bob@example.com>".into()],
            subject: "Hello".into(),
            body: "Hi".into(),
            ..Default::default()
        }
    }

    #[test]
    fn composes() {
        let m = compose("me@example.com", &mail()).unwrap();
        let raw = String::from_utf8(m.formatted()).unwrap();
        assert!(raw.contains("Subject: Hello"));
        assert!(raw.contains("bob@example.com"));
    }

    #[test]
    fn rejects_bad_input() {
        let mut m = mail();
        m.to.clear();
        assert!(compose("me@example.com", &m).is_err());
        let mut m = mail();
        m.subject = "x\r\nBcc: evil@example.com".into();
        assert!(compose("me@example.com", &m).is_err());
        let mut m = mail();
        m.to = vec!["not an address".into()];
        assert!(compose("me@example.com", &m).is_err());
        let mut m = mail();
        m.to = (0..51).map(|i| format!("u{i}@example.com")).collect();
        assert!(compose("me@example.com", &m).is_err());
    }
}
