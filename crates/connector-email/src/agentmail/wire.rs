//! Request bodies and response shapes of the `AgentMail` REST API, and their
//! conversion to the connector's message types. Unknown response fields are
//! ignored; every string that came from the network is length-capped.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{EmailError, ProviderError};
use crate::message::{
    AttachmentInfo, MAX_BODY_CHARS, MAX_MESSAGE_BYTES, MessageDetail, MessageSummary,
    cap_untrusted, truncate_chars,
};
use crate::smtp::OutgoingMail;

#[derive(Debug, Deserialize)]
pub(super) struct InboxWire {
    pub inbox_id: String,
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ListWire {
    #[serde(default)]
    pub messages: Vec<MessageWire>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachmentWire {
    pub attachment_id: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessageWire {
    pub message_id: String,
    /// The thread the message is in.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// `In-Reply-To` (one id, or a list: not documented as either, so both are read).
    #[serde(default)]
    pub in_reply_to: Option<Value>,
    /// `References`.
    #[serde(default)]
    pub references: Option<Value>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentWire>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachmentMetaWire {
    pub download_url: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SendWire {
    pub message_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, EmailError> {
    serde_json::from_value(v).map_err(|_| ProviderError::BadResponse.into())
}

/// An id the provider handed out. Unlike free text it must come back whole (a
/// cut-off id names another resource), so it is capped at [`super::MAX_ID_LEN`],
/// not at the 256 characters of other untrusted fields.
pub(super) fn cap_id(id: &str) -> String {
    truncate_chars(id, super::MAX_ID_LEN).0
}

/// Message ids out of a JSON string (maybe several, separated by spaces) or list of strings,
/// without angle brackets. Anything else is nothing.
fn id_list(v: Option<&Value>) -> Vec<String> {
    let texts: Vec<&str> = match v {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(|id| id.trim_matches(['<', '>']))
        .filter(|id| !id.is_empty())
        .take(50)
        .map(cap_id)
        .collect()
}

fn has_label(m: &MessageWire, label: &str) -> bool {
    m.labels.iter().any(|l| l.eq_ignore_ascii_case(label))
}

impl MessageWire {
    /// Whether the provider filed this message as spam. Label names are not a
    /// documented closed set, so both common spellings count.
    pub(super) fn is_junk(&self) -> bool {
        has_label(self, "spam") || has_label(self, "junk")
    }

    /// Whether this is mail the inbox received (not one it sent or a draft).
    /// Label names are not documented as a closed set, so this errs towards
    /// keeping a message: only `sent`/`draft` labels and the inbox's own
    /// address as sender exclude one.
    pub(super) fn is_inbound(&self, own_address: Option<&str>) -> bool {
        if has_label(self, "sent") || has_label(self, "draft") {
            return false;
        }
        let from = self.from.to_ascii_lowercase();
        own_address.is_none_or(|a| {
            let a = a.to_ascii_lowercase();
            !(from == a || from.ends_with(&format!("<{a}>")))
        })
    }

    pub(super) fn summary(&self) -> MessageSummary {
        MessageSummary {
            uid: 0,
            id: Some(cap_id(&self.message_id)),
            subject: cap_untrusted(self.subject.as_deref().unwrap_or_default()),
            from: cap_untrusted(&self.from),
            from_addr: crate::message::bare_address(&self.from),
            to: self.to.iter().map(|s| cap_untrusted(s)).collect(),
            date: self.timestamp.as_deref().map(cap_untrusted),
            unread: has_label(self, "unread"),
            size: self.size.and_then(|s| u32::try_from(s).ok()),
            automation_headers: None,
            has_attachment: Some(!self.attachments.is_empty()),
            folder: None,
            from_junk: Some(self.is_junk()),
        }
    }

    pub(super) fn detail(&self, max_body_chars: usize) -> Result<MessageDetail, EmailError> {
        if self
            .size
            .is_some_and(|s| s > u64::from(MAX_MESSAGE_BYTES) * 4)
        {
            return Err(ProviderError::BadResponse.into());
        }
        let body = match (&self.text, &self.html) {
            (Some(t), _) if !t.trim().is_empty() => t.clone(),
            (_, Some(h)) => html_to_text(h),
            _ => self.preview.clone().unwrap_or_default(),
        };
        let (body_text, body_truncated) = truncate_chars(&body, max_body_chars.min(MAX_BODY_CHARS));
        let links = crate::verify::merge_links(&[
            self.html
                .as_deref()
                .map(crate::verify::hrefs_in_html)
                .unwrap_or_default(),
            self.text
                .as_deref()
                .map(crate::verify::urls_in_text)
                .unwrap_or_default(),
        ]);
        Ok(MessageDetail {
            summary: self.summary(),
            cc: self.cc.iter().map(|s| cap_untrusted(s)).collect(),
            message_id: Some(cap_id(&self.message_id)),
            body_text,
            body_truncated,
            links,
            attachments: self
                .attachments
                .iter()
                .map(|a| AttachmentInfo {
                    id: Some(cap_id(&a.attachment_id)),
                    filename: a.filename.as_deref().map(cap_untrusted),
                    content_type: a
                        .content_type
                        .as_deref()
                        .map_or_else(|| "application/octet-stream".into(), cap_untrusted),
                    size: usize::try_from(a.size.unwrap_or(0)).unwrap_or(usize::MAX),
                })
                .collect(),
            reply_to: None,
            references: id_list(self.references.as_ref()),
            in_reply_to: id_list(self.in_reply_to.as_ref()),
            thread_id: self.thread_id.as_deref().map(cap_id),
            // The API gives no delivery-status part; the body text is read for one instead.
            delivery_status: None,
            recipients: self
                .to
                .iter()
                .chain(&self.cc)
                .map(|a| crate::message::bare_address(a))
                .filter(|a| !a.is_empty())
                .collect(),
            found_in: None,
            from_junk: Some(self.is_junk()),
            // The API reports no SPF/DKIM result.
            sender_authenticated: None,
        })
    }
}

/// Plain text of an HTML body (links and images dropped).
fn html_to_text(html: &str) -> String {
    let opts = web_tools::ExtractOptions {
        include_links: false,
        include_images: false,
        full_page: true,
        max_links: 0,
    };
    web_tools::html_to_markdown(html, None, &opts).markdown
}

/// Local part for a new inbox from a free-form hint: lower-case ASCII letters,
/// digits and single hyphens, at most 32 characters. `None` when nothing is
/// left (the provider then picks a random one).
pub(super) fn username_from_hint(hint: &str) -> Option<String> {
    let mut out = String::new();
    for c in hint.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if matches!(c, '-' | '_' | ' ' | '.') && !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    let out: String = out.chars().take(32).collect();
    let out = out.trim_end_matches('-').to_owned();
    (!out.is_empty()).then_some(out)
}

pub(super) fn send_body(mail: &OutgoingMail) -> Value {
    let mut body = json!({
        "to": mail.to,
        "subject": mail.subject,
        "text": mail.body,
    });
    if !mail.cc.is_empty() {
        body["cc"] = json!(mail.cc);
    }
    if !mail.bcc.is_empty() {
        body["bcc"] = json!(mail.bcc);
    }
    body
}

pub(super) fn reply_body(mail: &OutgoingMail, reply_all: bool) -> Value {
    let mut body = json!({ "text": mail.body });
    if !mail.to.is_empty() {
        body["to"] = json!(mail.to);
    }
    if !mail.cc.is_empty() {
        body["cc"] = json!(mail.cc);
    }
    if !mail.bcc.is_empty() {
        body["bcc"] = json!(mail.bcc);
    }
    if reply_all {
        body["reply_all"] = json!(true);
    }
    body
}

/// `YYYY-MM-DD` to an RFC 3339 timestamp at 00:00:00Z (validated like the IMAP
/// dates).
pub(super) fn day_start(date: &str) -> Result<String, EmailError> {
    crate::message::imap_date(date)?; // validates the calendar date
    Ok(format!("{date}T00:00:00Z"))
}

/// Whether `s` is a plain `local@domain` address: ASCII only, no display name,
/// comment, quoting, routing or whitespace. The provider is sent exactly this
/// string, so it is also exactly what the approval card shows: there is no
/// second parser that could read it as a different recipient.
pub(super) fn is_plain_address(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'\'' | b'-'));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    local_ok && domain_ok && s.len() <= 254
}
