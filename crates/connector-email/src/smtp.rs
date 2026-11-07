//! Message composition and SMTP submission with mandatory verified TLS.

use mail_builder::MessageBuilder;
use mail_parser::MessageParser;
use mail_send::SmtpClientBuilder;
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

fn mailbox(s: &str) -> Result<(String, String), EmailError> {
    let bad = || EmailError::Input(format!("invalid address {s:?}"));
    if s.contains(['\r', '\n', '\0']) {
        return Err(bad());
    }
    let header = format!("From: {s}\r\n\r\n");
    let msg = MessageParser::default()
        .parse(header.as_bytes())
        .ok_or_else(bad)?;
    let addresses = msg.from().ok_or_else(bad)?;
    let mut iter = addresses.iter();
    let address = iter.next().ok_or_else(bad)?;
    let email = address.address.as_deref().ok_or_else(bad)?;
    if iter.next().is_some() || email.parse::<email_address::EmailAddress>().is_err() {
        return Err(bad());
    }
    Ok((
        address.name.as_deref().unwrap_or_default().to_owned(),
        email.to_owned(),
    ))
}

/// Composed message and separate delivery envelope. Blind recipients never
/// appear in transmitted headers.
pub struct ComposedMail {
    raw: Vec<u8>,
    from: String,
    recipients: Vec<String>,
}

impl ComposedMail {
    /// RFC 5322 message bytes.
    #[must_use]
    pub fn formatted(&self) -> Vec<u8> {
        self.raw.clone()
    }
}

/// Compose an RFC 5322 message.
///
/// # Errors
/// [`EmailError::Input`] for invalid or missing recipients, oversized bodies, or
/// line breaks in the subject.
pub fn compose(from: &str, mail: &OutgoingMail) -> Result<ComposedMail, EmailError> {
    compose_message(from, mail, false)
}

pub(crate) fn compose_draft(from: &str, mail: &OutgoingMail) -> Result<ComposedMail, EmailError> {
    compose_message(from, mail, true)
}

fn compose_message(
    from: &str,
    mail: &OutgoingMail,
    draft: bool,
) -> Result<ComposedMail, EmailError> {
    let total = mail.to.len() + mail.cc.len() + mail.bcc.len();
    if total == 0 {
        return Err(EmailError::Input(
            "at least one recipient is required".into(),
        ));
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
    let from = mailbox(from)?;
    let to = mail
        .to
        .iter()
        .map(|s| mailbox(s))
        .collect::<Result<Vec<_>, _>>()?;
    let cc = mail
        .cc
        .iter()
        .map(|s| mailbox(s))
        .collect::<Result<Vec<_>, _>>()?;
    let bcc = mail
        .bcc
        .iter()
        .map(|s| mailbox(s))
        .collect::<Result<Vec<_>, _>>()?;
    let recipients = to
        .iter()
        .chain(&cc)
        .chain(&bcc)
        .map(|(_, email)| email.clone())
        .collect();
    let mut builder = MessageBuilder::new()
        .from(from.clone())
        .subject(mail.subject.clone())
        .text_body(mail.body.clone());
    if !to.is_empty() {
        builder = builder.to(to);
    }
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    if draft && !bcc.is_empty() {
        builder = builder.bcc(bcc);
    }
    let raw = builder
        .write_to_vec()
        .map_err(|e| EmailError::Input(format!("cannot build message: {e}")))?;
    Ok(ComposedMail {
        raw,
        from: from.1,
        recipients,
    })
}

/// Submit a composed message. TLS is always required; there is no plaintext path.
///
/// # Errors
/// TLS, authentication or delivery failures.
pub async fn send(
    acct: &AccountConfig,
    password: &str,
    msg: ComposedMail,
) -> Result<String, EmailError> {
    let map_error = |e: mail_send::Error| {
        // Server authentication replies can echo encoded credentials.
        if matches!(
            e,
            mail_send::Error::AuthenticationFailed(_) | mail_send::Error::Auth(_)
        ) {
            EmailError::Smtp("authentication failed".into())
        } else {
            EmailError::Smtp(e.to_string().replace(password, "[REDACTED]"))
        }
    };
    let mut builder = SmtpClientBuilder::new(acct.smtp.host.clone(), acct.smtp.port)
        .implicit_tls(acct.smtp.security == SmtpSecurity::Implicit)
        .credentials((acct.login().to_owned(), password.to_owned()))
        .timeout(acct.timeout());
    builder.tls_connector = crate::tls::connector(acct.extra_root_pem.as_deref())?;
    let mut client = builder.connect().await.map_err(map_error)?;
    let envelope = mail_send::smtp::message::Message::new(msg.from, msg.recipients, msg.raw);
    client.send(envelope).await.map_err(map_error)?;
    // DATA acceptance is success. A QUIT failure must not invite duplicate delivery.
    let _ = client.quit().await;
    Ok("SMTP server accepted message".into())
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
