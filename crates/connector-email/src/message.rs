//! Message models and RFC 5322 parsing (via `mail-parser`).

use mail_parser::{Address, MessageParser, MimeHeaders};
use serde::Serialize;

use crate::error::EmailError;

/// Maximum body characters returned by default.
pub const DEFAULT_MAX_BODY_CHARS: usize = 20_000;

/// Header-level view of a message (search results).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageSummary {
    /// IMAP UID within the folder.
    pub uid: u32,
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

/// Attachment metadata (content is never returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentInfo {
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
        subject,
        from,
        to,
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
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| EmailError::Imap("message could not be parsed".into()))?;
    let summary = parse_summary(uid, raw, unread, size);
    let body = msg
        .body_text(0)
        .map(|b| b.to_string())
        .unwrap_or_default();
    let (body_text, body_truncated) = truncate_chars(&body, max_body_chars);
    let attachments = msg
        .attachments()
        .map(|p| AttachmentInfo {
            filename: p.attachment_name().map(str::to_string),
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
        cc: addresses(msg.cc()),
        message_id: msg.message_id().map(str::to_string),
        body_text,
        body_truncated,
        attachments,
    })
}

fn truncate_chars(s: &str, max: usize) -> (String, bool) {
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
    let y: u32 = y.parse().map_err(|_| bad())?;
    let m: usize = m.parse().map_err(|_| bad())?;
    let d: u32 = d.parse().map_err(|_| bad())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
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
}
