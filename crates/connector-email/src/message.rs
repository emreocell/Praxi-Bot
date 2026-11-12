//! Message models and RFC 5322 parsing (via `mail-parser`).

use mail_parser::{Address, MessageParser, MimeHeaders};
use serde::Serialize;

use crate::error::EmailError;

/// Maximum body characters returned by default.
pub const DEFAULT_MAX_BODY_CHARS: usize = 20_000;
/// Hard ceiling for `max_body_chars`. Callers cannot raise the returned body above this.
pub const MAX_BODY_CHARS: usize = DEFAULT_MAX_BODY_CHARS;
/// Refuse to fetch or parse a message (body and attachments) larger than this.
pub const MAX_MESSAGE_BYTES: u32 = 1024 * 1024;
/// Maximum characters kept from a server-controlled subject, sender, or file name.
pub const MAX_UNTRUSTED_FIELD_CHARS: usize = 256;

pub(crate) fn over_message_cap(len: usize) -> bool {
    u64::try_from(len).unwrap_or(u64::MAX) > u64::from(MAX_MESSAGE_BYTES)
}

/// Header-level view of a message (search results).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageSummary {
    /// IMAP UID within the folder (`0`, and not serialized, for mailboxes
    /// that identify messages by [`MessageSummary::id`]).
    #[serde(skip_serializing_if = "is_zero")]
    pub uid: u32,
    /// The provider's message id (API-backed mailboxes; absent for IMAP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Subject line.
    pub subject: String,
    /// Sender (`Name <addr>`).
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Date header as RFC 3339, when parseable.
    pub date: Option<String>,
    /// `true` when `\Seen` is not set.
    pub unread: bool,
    /// Size in bytes (RFC822.SIZE).
    pub size: Option<u32>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Attachment metadata (content is never returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentInfo {
    /// The provider's attachment id (API-backed mailboxes; absent for IMAP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// File name, when given.
    pub filename: Option<String>,
    /// MIME type.
    pub content_type: String,
    /// Decoded size in bytes.
    pub size: usize,
}

/// A fully fetched message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageDetail {
    /// Header-level fields.
    #[serde(flatten)]
    pub summary: MessageSummary,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// `Message-ID` header.
    pub message_id: Option<String>,
    /// Plain-text body (HTML is converted). Untrusted content.
    pub body_text: String,
    /// `true` when `body_text` was cut at the limit.
    pub body_truncated: bool,
    /// Attachment metadata.
    pub attachments: Vec<AttachmentInfo>,
}

fn addresses(a: Option<&Address<'_>>) -> Vec<String> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .filter_map(|addr| {
            let email = addr.address.as_deref()?;
            Some(match addr.name.as_deref() {
                Some(n) if !n.is_empty() => format!("{n} <{email}>"),
                _ => email.to_string(),
            })
        })
        .collect()
}

/// Parse the header block of a message into a summary.
#[must_use]
pub fn parse_summary(uid: u32, header: &[u8], unread: bool, size: Option<u32>) -> MessageSummary {
    let msg = MessageParser::default().parse(header);
    let (subject, from, to, date) = match &msg {
        Some(m) => (
            m.subject().unwrap_or_default().to_string(),
            addresses(m.from()).into_iter().next().unwrap_or_default(),
            addresses(m.to()),
            m.date().map(mail_parser::DateTime::to_rfc3339),
        ),
        None => (String::new(), String::new(), Vec::new(), None),
    };
    MessageSummary {
        uid,
        id: None,
        subject: cap_untrusted(&subject),
        from: cap_untrusted(&from),
        to: to.iter().map(|s| cap_untrusted(s)).collect(),
        date,
        unread,
        size,
    }
}

/// Parse a full message.
///
/// # Errors
/// [`EmailError::Imap`] if the bytes are not a parseable message.
pub fn parse_detail(
    uid: u32,
    raw: &[u8],
    unread: bool,
    size: Option<u32>,
    max_body_chars: usize,
) -> Result<MessageDetail, EmailError> {
    if over_message_cap(raw.len()) {
        return Err(EmailError::Imap(format!(
            "message uid {uid} is {} bytes, over the {MAX_MESSAGE_BYTES} byte cap",
            raw.len()
        )));
    }
    let max_body_chars = max_body_chars.min(MAX_BODY_CHARS);
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| EmailError::Imap("message could not be parsed".into()))?;
    let summary = parse_summary(uid, raw, unread, size);
    let body = msg.body_text(0).map(|b| b.to_string()).unwrap_or_default();
    let (body_text, body_truncated) = truncate_chars(&body, max_body_chars);
    let attachments = msg
        .attachments()
        .map(|p| AttachmentInfo {
            id: None,
            filename: p.attachment_name().map(cap_untrusted),
            content_type: p.content_type().map_or_else(
                || "application/octet-stream".to_string(),
                |ct| match ct.subtype() {
                    Some(s) => format!("{}/{s}", ct.ctype()),
                    None => ct.ctype().to_string(),
                },
            ),
            size: p.len(),
        })
        .collect();
    Ok(MessageDetail {
        summary,
        cc: addresses(msg.cc())
            .iter()
            .map(|s| cap_untrusted(s))
            .collect(),
        message_id: msg.message_id().map(cap_untrusted),
        body_text,
        body_truncated,
        attachments,
    })
}

pub(crate) fn cap_untrusted(s: &str) -> String {
    truncate_chars(s, MAX_UNTRUSTED_FIELD_CHARS).0
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (s[..i].to_string(), true),
        None => (s.to_string(), false),
    }
}

/// Convert `YYYY-MM-DD` to the IMAP `D-Mon-YYYY` date format.
///
/// # Errors
/// [`EmailError::Input`] for anything that is not a plausible calendar date.
pub fn imap_date(s: &str) -> Result<String, EmailError> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let bad = || EmailError::Input(format!("date must be YYYY-MM-DD, got {s:?}"));
    let mut it = s.split('-');
    let (Some(y), Some(m), Some(d), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err(bad());
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return Err(bad());
    }
    if ![y, m, d]
        .iter()
        .all(|s| s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(bad());
    }
    let y: u32 = y.parse().map_err(|_| bad())?;
    let m: usize = m.parse().map_err(|_| bad())?;
    let d: u32 = d.parse().map_err(|_| bad())?;
    if !(1..=12).contains(&m) || y == 0 {
        return Err(bad());
    }
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days).contains(&d) {
        return Err(bad());
    }
    Ok(format!("{d}-{}-{y}", MONTHS[m - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_convert() {
        assert_eq!(imap_date("2026-11-07").unwrap(), "7-Nov-2026");
        assert!(imap_date("2026-13-07").is_err());
        assert!(imap_date("2026-02-29").is_err());
        assert!(imap_date("1900-02-29").is_err());
        assert!(imap_date("2026-04-31").is_err());
        assert_eq!(imap_date("2000-02-29").unwrap(), "29-Feb-2000");
        assert!(imap_date("07/11/2026").is_err());
        assert!(imap_date("2026-11-07\r\nA1 DELETE x").is_err());
    }

    #[test]
    fn parses_multipart_with_attachment() {
        let raw = b"From: Ann <ann@example.com>\r\nTo: bob@example.com\r\nSubject: Hi\r\n\
Message-ID: <1@x>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n\
--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello world\r\n\
--B\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\nContent-Disposition: attachment; filename=\"a.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--B--\r\n";
        let d = parse_detail(7, raw, true, Some(100), 1000).unwrap();
        assert_eq!(d.summary.subject, "Hi");
        assert_eq!(d.summary.from, "Ann <ann@example.com>");
        assert_eq!(d.body_text.trim(), "Hello world");
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].filename.as_deref(), Some("a.pdf"));
        assert_eq!(d.attachments[0].content_type, "application/pdf");
        assert_eq!(d.attachments[0].size, 4);
    }

    #[test]
    fn body_is_truncated() {
        let raw = b"Subject: x\r\n\r\nabcdefghij\r\n";
        let d = parse_detail(1, raw, false, None, 4).unwrap();
        assert_eq!(d.body_text, "abcd");
        assert!(d.body_truncated);
    }

    #[test]
    fn html_fallback_and_unicode_truncation() {
        let raw =
            "Subject: html\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Merhaba dünya</p>";
        let d = parse_detail(1, raw.as_bytes(), true, None, 10).unwrap();
        assert!(!d.body_text.contains("<p>"));
        assert_eq!(d.body_text.chars().count(), 10);
        assert!(d.body_truncated);
    }

    #[test]
    fn max_body_chars_is_clamped() {
        let body = "a".repeat(MAX_BODY_CHARS + 80);
        let raw = format!("Subject: s\r\n\r\n{body}\r\n");
        let d = parse_detail(
            1,
            raw.as_bytes(),
            false,
            u32::try_from(raw.len()).ok(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(d.body_text.chars().count(), MAX_BODY_CHARS);
        assert!(d.body_truncated);
    }

    #[test]
    fn oversized_payload_is_refused_before_parse() {
        let mut raw = b"Subject: hi\r\n\r\n".to_vec();
        let extra = usize::try_from(MAX_MESSAGE_BYTES).unwrap_or(1024 * 1024);
        raw.extend(std::iter::repeat_n(b'A', extra));
        let err = parse_detail(1, &raw, false, Some(u32::MAX), usize::MAX).unwrap_err();
        assert!(err.to_string().contains("byte cap"), "{err}");
        assert!(!err.to_string().contains("could not be parsed"), "{err}");
    }

    #[test]
    fn subject_from_and_filename_are_length_capped() {
        let long = "n".repeat(MAX_UNTRUSTED_FIELD_CHARS + 40);
        let raw = format!(
            "From: {long} <ann@example.com>\r\nSubject: {long}\r\n\
MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n\
--B\r\nContent-Type: text/plain\r\n\r\nHi\r\n\
--B\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"{long}.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--B--\r\n"
        );
        let d = parse_detail(1, raw.as_bytes(), true, Some(100), 100).unwrap();
        assert_eq!(d.summary.subject.chars().count(), MAX_UNTRUSTED_FIELD_CHARS);
        assert_eq!(d.summary.from.chars().count(), MAX_UNTRUSTED_FIELD_CHARS);
        assert_eq!(
            d.attachments[0].filename.as_ref().unwrap().chars().count(),
            MAX_UNTRUSTED_FIELD_CHARS
        );
    }
}
