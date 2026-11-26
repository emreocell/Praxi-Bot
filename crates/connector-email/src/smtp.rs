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
    /// `Message-ID` of the mail this one answers. Set by the daemon for a reply
    /// (read from the original), never taken from the model.
    #[serde(skip)]
    pub in_reply_to: Option<String>,
    /// `References` chain of a reply: the original's chain plus its own id.
    #[serde(skip)]
    pub references: Vec<String>,
    /// A calendar invitation to attach. Set by the calendar connector, never
    /// read from a tool's input.
    #[serde(skip)]
    pub calendar: Option<CalendarPart>,
}

/// An iCalendar message (an invitation or a cancellation) sent with a mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarPart {
    /// The iTIP method: `REQUEST`, `CANCEL`.
    pub method: String,
    /// The `.ics` document.
    pub ics: String,
}

/// Models sometimes write a line break as the two characters backslash and `n` instead of a
/// real line break, and the mail then shows them between the paragraphs. When `text` has NO
/// real line break but does contain such escape sequences, they are turned into the characters
/// they stand for (backslash-r-backslash-n, backslash-n, backslash-r, backslash-t). A text that
/// already has a real line break is never touched. `None`: nothing to fix.
#[must_use]
pub fn unescape_literal_breaks(text: &str) -> Option<String> {
    if text.contains(['\n', '\r']) || !text.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                out.push('\n');
                changed = true;
            }
            Some('t') => {
                chars.next();
                out.push('\t');
                changed = true;
            }
            Some('r') => {
                chars.next();
                out.push('\r');
                changed = true;
                let mut ahead = chars.clone();
                if ahead.next() == Some('\\') && ahead.next() == Some('n') {
                    chars.next();
                    chars.next();
                    out.push('\n');
                }
            }
            // An escaped backslash, or a lone one, stays as the model wrote it.
            _ => out.push(c),
        }
    }
    changed.then_some(out)
}

impl OutgoingMail {
    /// Fix literal backslash-n / backslash-r-backslash-n / backslash-t sequences (see
    /// [`unescape_literal_breaks`]) in the body and the subject. A subject must stay one line,
    /// so an escaped line break there becomes a space.
    pub fn fix_literal_breaks(&mut self) {
        if let Some(fixed) = unescape_literal_breaks(&self.body) {
            self.body = fixed;
        }
        if let Some(fixed) = unescape_literal_breaks(&self.subject) {
            self.subject = fixed
                .split(['\r', '\n', '\t'])
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
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

/// The address that goes into the SMTP envelope for `s` (`a@b` or `Name <a@b>`): the very parse
/// [`compose`] uses, so the outgoing-mail guard reads the same recipient that is delivered.
///
/// # Errors
/// [`EmailError::Input`] when `s` is not one valid mailbox.
pub fn envelope_address(s: &str) -> Result<String, EmailError> {
    mailbox(s).map(|(_, email)| email)
}

/// A `Message-ID` for a threading header, without the angle brackets; `None` for
/// an empty one or one that cannot be written into a header as it is: more than
/// 998 characters, or anything but printable ASCII without `<`, `>`, `"` and `\`
/// (whitespace, control and non-ASCII characters can split or smuggle a header).
pub(crate) fn valid_threading_id(id: &str) -> Option<String> {
    let id = id.trim().trim_start_matches('<').trim_end_matches('>');
    (!id.is_empty()
        && id.len() <= 998
        && id
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '\\')))
    .then(|| id.to_owned())
}

/// [`valid_threading_id`] for an id the caller of [`compose`] supplied: `None` for an absent
/// one, an error for one that is not safe to write.
fn threading_id(id: Option<&str>) -> Result<Option<String>, EmailError> {
    let Some(raw) = id
        .map(str::trim)
        .filter(|i| !i.trim_matches(['<', '>']).is_empty())
    else {
        return Ok(None);
    };
    valid_threading_id(raw)
        .map(Some)
        .ok_or_else(|| EmailError::Input("invalid Message-ID".into()))
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
    if mail.subject.chars().any(char::is_control) {
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
    if let Some(cal) = &mail.calendar {
        if !cal.method.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(EmailError::Input("bad calendar method".into()));
        }
        // Mail programs recognise an invitation by the `method` parameter.
        builder = builder.attachment(
            format!("text/calendar; method={}; charset=UTF-8", cal.method),
            "invite.ics",
            cal.ics.clone().into_bytes(),
        );
    }
    if !to.is_empty() {
        builder = builder.to(to);
    }
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    if draft && !bcc.is_empty() {
        builder = builder.bcc(bcc);
    }
    if let Some(id) = threading_id(mail.in_reply_to.as_deref())? {
        builder = builder.in_reply_to(id);
    }
    let references = mail
        .references
        .iter()
        .map(|r| threading_id(Some(r)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if !references.is_empty() {
        builder = builder.references(references);
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

/// `text` with every copy of `password` blanked. An empty password is skipped: replacing
/// "nothing" would put the marker between every two characters.
fn redact_password(text: &str, password: &str) -> String {
    if password.is_empty() {
        text.to_owned()
    } else {
        text.replace(password, "[REDACTED]")
    }
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
            EmailError::Smtp(redact_password(&e.to_string(), password))
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
    fn an_empty_password_redacts_nothing_and_a_real_one_is_blanked() {
        assert_eq!(
            redact_password("550 mailbox unavailable", ""),
            "550 mailbox unavailable"
        );
        assert_eq!(
            redact_password("bad login hunter2 for hunter2", "hunter2"),
            "bad login [REDACTED] for [REDACTED]"
        );
    }

    #[test]
    fn composes() {
        let m = compose("me@example.com", &mail()).unwrap();
        let raw = String::from_utf8(m.formatted()).unwrap();
        assert!(raw.contains("Subject: Hello"));
        assert!(raw.contains("bob@example.com"));
    }

    #[test]
    fn a_calendar_part_is_attached_as_text_calendar_with_its_method() {
        let mut m = mail();
        m.calendar = Some(CalendarPart {
            method: "REQUEST".into(),
            ics: "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".into(),
        });
        let raw = String::from_utf8(compose("me@example.com", &m).unwrap().formatted()).unwrap();
        assert!(raw.contains("text/calendar"), "{raw}");
        assert!(raw.contains("method=REQUEST"), "{raw}");
        assert!(raw.contains("invite.ics"), "{raw}");
        // A method that could carry a header break is refused.
        m.calendar = Some(CalendarPart {
            method: "REQUEST\r\nBcc: x@example.com".into(),
            ics: String::new(),
        });
        assert!(compose("me@example.com", &m).is_err());
    }

    #[test]
    fn cr_lf_and_nul_in_to_cc_and_subject_are_refused() {
        for bad in [
            "a\rb",
            "a\nb",
            "a\0b",
            "x@example.com\r\nBcc: evil@example.com",
        ] {
            let mut m = mail();
            m.subject = bad.into();
            assert!(compose("me@example.com", &m).is_err(), "subject {bad:?}");
            let mut m = mail();
            m.to = vec![bad.into()];
            assert!(compose("me@example.com", &m).is_err(), "to {bad:?}");
            let mut m = mail();
            m.cc = vec![bad.into()];
            assert!(compose("me@example.com", &m).is_err(), "cc {bad:?}");
        }
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

#[cfg(test)]
mod break_tests {
    use super::*;

    #[test]
    fn escaped_breaks_are_fixed_only_when_there_is_no_real_break() {
        assert_eq!(
            unescape_literal_breaks(r"Hi,\n\nBye\r\nNow\tgo").as_deref(),
            Some("Hi,\n\nBye\r\nNow\tgo")
        );
        assert_eq!(unescape_literal_breaks("plain text"), None);
        // A real line break anywhere: left alone, escapes included.
        assert_eq!(unescape_literal_breaks("a\\nb\nc"), None);
        // A backslash that does not start an escape is not a break.
        assert_eq!(unescape_literal_breaks(r"C:\folder"), None);
    }

    #[test]
    fn the_subject_stays_on_one_line() {
        let mut mail = OutgoingMail {
            subject: r"Hello\nthere".into(),
            body: r"a\nb".into(),
            ..Default::default()
        };
        mail.fix_literal_breaks();
        assert_eq!(mail.subject, "Hello there");
        assert_eq!(mail.body, "a\nb");
    }
}
