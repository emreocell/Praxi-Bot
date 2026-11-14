//! `AgentMail` over its REST API with the user's own API key.
//!
//! Endpoints used (all under `https://api.agentmail.to`, see the PR for the doc
//! pages each shape came from): `POST /v0/inboxes`, `DELETE /v0/inboxes/{id}`,
//! `GET /v0/inboxes` and `GET /v0/inboxes/{id}` (cheap authenticated checks),
//! `GET /v0/inboxes/{id}/messages`, `GET|PATCH .../messages/{mid}`,
//! `GET .../messages/{mid}/attachments/{aid}`, `POST .../messages/send` and
//! `POST .../messages/{mid}/reply`.

pub(crate) mod http;
mod wire;

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};

use crate::config::AccountKind;
use crate::error::{EmailError, ProviderError};
use crate::imap::FolderInfo;
use crate::message::{MessageDetail, cap_untrusted};
use crate::provider::{
    AttachmentData, Capabilities, CreateInbox, Found, InboxInfo, MailboxProvider, NewMessages,
    Receipt, SearchRequest,
};
use crate::smtp::{self, OutgoingMail};

pub use http::{AgentMailClient, AgentMailEndpoint};
use wire::{
    AttachmentMetaWire, InboxWire, ListWire, MessageWire, SendWire, day_start, parse, reply_body,
    send_body, username_from_hint,
};

/// Longest id (inbox, message, attachment) accepted in a URL path.
pub const MAX_ID_LEN: usize = 320;

/// Page size limit of the list endpoint (filtered requests are capped at 100).
const MAX_LIMIT: usize = 100;
const PAGE_CURSOR_PREFIX: &str = "agentmail-page-v1:";
const MAX_PAGE_TOKEN: usize = 4096;
const MAX_PAGE_CURSOR: usize = 16 * 1024;
/// Automatic attempts for an idempotent create.
const CREATE_ATTEMPTS: u32 = 3;
/// Longest pause taken for a `Retry-After` before retrying a create.
const MAX_AUTO_WAIT_SECS: u64 = 10;

fn id_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b'-' | b'@' | b'+' | b'=' | b'~' | b':' | b'<' | b'>'
        )
}

/// Check an id that will be placed in a URL path: ASCII letters, digits and
/// `. _ - @ + = ~ : < >` (message ids look like `<abc@agentmail.to>`), at most
/// 320 characters, never `.` or `..`. Path separators, spaces, `%`, `?` and `#`
/// are refused, so an id can never change which resource a URL names.
///
/// # Errors
/// [`EmailError::Config`] when the id is empty or contains anything else.
pub fn check_id(id: &str, what: &str) -> Result<(), EmailError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id != "."
        && id != ".."
        && id.bytes().all(id_byte_ok);
    if ok {
        Ok(())
    } else {
        Err(EmailError::Config(format!("invalid {what}")))
    }
}

fn bad_id(e: EmailError) -> EmailError {
    match e {
        EmailError::Config(m) => EmailError::Input(m),
        other => other,
    }
}

fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// A cursor is a timestamp the provider handed out earlier.
fn valid_cursor(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 40
        && c.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'T' | b'Z' | b':' | b'.' | b'+' | b'-'))
        && mail_parser::DateTime::parse_rfc3339(c).is_some()
}

fn epoch(ts: &str) -> Option<i64> {
    mail_parser::DateTime::parse_rfc3339(ts).map(|d| d.to_timestamp())
}

/// Keep the original query watermark while draining a provider page sequence.
/// Only after its final page may a timestamp become the next polling cursor.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PageCursor {
    after: String,
    newest: String,
    page_token: String,
}

fn valid_page_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_PAGE_TOKEN
        && token.bytes().all(|b| b.is_ascii_graphic())
}

fn page_cursor(cursor: Option<&str>) -> Result<Option<PageCursor>, EmailError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if valid_cursor(cursor) {
        return Ok(None);
    }
    if cursor.len() <= MAX_PAGE_CURSOR
        && let Some(encoded) = cursor.strip_prefix(PAGE_CURSOR_PREFIX)
        && let Ok(page) = serde_json::from_str::<PageCursor>(encoded)
        && valid_cursor(&page.after)
        && valid_cursor(&page.newest)
        && valid_page_token(&page.page_token)
    {
        return Ok(Some(page));
    }
    Err(EmailError::Input("invalid cursor".into()))
}

/// The user's `AgentMail` API key. `Debug` never shows it.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap a key, checking only its shape (printable ASCII without
    /// whitespace, 8 to 512 bytes). The value is never echoed in the error.
    ///
    /// # Errors
    /// [`EmailError::Input`] for a key that cannot be an HTTP bearer token.
    pub fn new(key: &str) -> Result<Self, EmailError> {
        let k = key.trim();
        let ok = (8..=512).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_graphic());
        if ok {
            Ok(Self(k.to_owned()))
        } else {
            Err(EmailError::Input(
                "the API key has an invalid format".into(),
            ))
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// An `AgentMail` account: the user's key, and the inbox it works on (absent
/// while only creating one).
#[derive(Debug, Clone)]
pub struct AgentMailProvider {
    client: AgentMailClient,
    key: ApiKey,
    inbox: Option<(String, String)>,
}

impl AgentMailProvider {
    /// Provider for `key`; `inbox` is `(inbox_id, address)` of the inbox to
    /// operate on.
    ///
    /// # Errors
    /// [`EmailError::Config`] for an invalid inbox id.
    pub fn new(
        client: AgentMailClient,
        key: ApiKey,
        inbox: Option<(String, String)>,
    ) -> Result<Self, EmailError> {
        if let Some((id, _)) = &inbox {
            check_id(id, "inbox id")?;
        }
        Ok(Self { client, key, inbox })
    }

    fn inbox_id(&self) -> Result<&str, EmailError> {
        self.inbox
            .as_ref()
            .map(|(id, _)| id.as_str())
            .ok_or_else(|| EmailError::Input("this account has no inbox".into()))
    }

    fn address(&self) -> Option<&str> {
        self.inbox.as_ref().map(|(_, a)| a.as_str())
    }

    async fn call(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
        extra: &[(&'static str, &str)],
    ) -> Result<Value, EmailError> {
        Ok(self
            .client
            .call(&self.key, method, segments, query, body, extra)
            .await?
            .body)
    }

    async fn list(&self, query: &[(&str, String)]) -> Result<Vec<MessageWire>, EmailError> {
        Ok(self.list_page(query).await?.messages)
    }

    async fn list_page(&self, query: &[(&str, String)]) -> Result<ListWire, EmailError> {
        let inbox = self.inbox_id()?;
        let v = self
            .call(
                Method::GET,
                &["v0", "inboxes", inbox, "messages"],
                query,
                None,
                &[],
            )
            .await?;
        parse(v)
    }

    /// Validate what will be sent before any request is made.
    fn check_outgoing(&self, mail: &OutgoingMail, need_recipient: bool) -> Result<(), EmailError> {
        let from = self.address().unwrap_or("bot@example.com");
        let any = !(mail.to.is_empty() && mail.cc.is_empty() && mail.bcc.is_empty());
        // What is validated is what is sent (and shown for approval): only plain
        // `local@domain` strings, never a form another parser could read as a
        // different recipient.
        for r in mail.to.iter().chain(&mail.cc).chain(&mail.bcc) {
            if !wire::is_plain_address(r) {
                return Err(EmailError::Input(
                    "each recipient must be a plain address like name@example.com (no display name, comments or quotes)"
                        .into(),
                ));
            }
        }
        if need_recipient || any {
            smtp::compose(from, mail).map(|_| ())
        } else if mail.body.len() > smtp::MAX_BODY_BYTES {
            Err(EmailError::Input("body is too large".into()))
        } else {
            Ok(())
        }
    }

    async fn post_send(&self, segments: &[&str], body: &Value) -> Result<Receipt, EmailError> {
        let key = uuid::Uuid::now_v7().to_string();
        let v = self
            .call(
                Method::POST,
                segments,
                &[],
                Some(body),
                &[("idempotency-key", key.as_str())],
            )
            .await?;
        let r: SendWire = parse(v)?;
        Ok(Receipt {
            id: cap_untrusted(&r.message_id),
            thread_id: r.thread_id.as_deref().map(cap_untrusted),
        })
    }
}

#[async_trait]
impl MailboxProvider for AgentMailProvider {
    fn kind(&self) -> AccountKind {
        AccountKind::AgentMail
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_inbox: true,
            delete_inbox: true,
            folders: false,
            search: true,
            mark_read: true,
            drafts: false,
            list_new: true,
            attachments: true,
            reply: true,
            push: false,
        }
    }

    async fn create_inbox(&self, req: &CreateInbox) -> Result<InboxInfo, EmailError> {
        if !valid_client_id(&req.client_id) {
            return Err(EmailError::Input(
                "client id must be 1-256 characters of letters, digits and - . _ ~".into(),
            ));
        }
        let mut body = json!({ "client_id": req.client_id });
        if let Some(name) = req.name_hint.as_deref().and_then(username_from_hint) {
            body["username"] = json!(name);
        }
        if let Some(domain) = req
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            crate::config::validate_host(domain, "inbox domain")
                .map_err(|_| EmailError::Input("invalid domain".into()))?;
            body["domain"] = json!(domain.to_ascii_lowercase());
        }
        let mut attempt = 0;
        let value = loop {
            attempt += 1;
            // The same `client_id` on every attempt: a retry after a lost
            // answer returns the first inbox instead of creating a second.
            match self
                .call(Method::POST, &["v0", "inboxes"], &[], Some(&body), &[])
                .await
            {
                Ok(v) => break v,
                Err(EmailError::Provider(ProviderError::RateLimited { retry_after_secs }))
                    if attempt < CREATE_ATTEMPTS =>
                {
                    let wait = retry_after_secs.unwrap_or(1).min(MAX_AUTO_WAIT_SECS);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
                Err(EmailError::Provider(ProviderError::Transient))
                    if attempt < CREATE_ATTEMPTS =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(attempt)))
                        .await;
                }
                Err(e) => return Err(e),
            }
        };
        let inbox: InboxWire = parse(value)?;
        let well_formed = check_id(&inbox.inbox_id, "inbox id").is_ok()
            && inbox.email.len() <= crate::config::MAX_LOGIN_LEN
            && inbox.email.parse::<email_address::EmailAddress>().is_ok();
        if !well_formed {
            return Err(ProviderError::BadResponse.into());
        }
        Ok(InboxInfo {
            inbox_id: inbox.inbox_id,
            address: inbox.email,
        })
    }

    async fn delete_inbox(&self) -> Result<(), EmailError> {
        let inbox = self.inbox_id()?;
        self.call(Method::DELETE, &["v0", "inboxes", inbox], &[], None, &[])
            .await
            .map(|_| ())
    }

    async fn check(&self) -> Result<(), EmailError> {
        match self.inbox_id() {
            Ok(inbox) => self
                .call(Method::GET, &["v0", "inboxes", inbox], &[], None, &[])
                .await
                .map(|_| ()),
            Err(_) => self
                .call(
                    Method::GET,
                    &["v0", "inboxes"],
                    &[("limit", "1".to_owned())],
                    None,
                    &[],
                )
                .await
                .map(|_| ()),
        }
    }

    async fn list_new(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<NewMessages, EmailError> {
        let page = page_cursor(cursor)?;
        let after = page.as_ref().map(|p| p.after.as_str()).or(cursor);
        let mut query = vec![("limit", limit.clamp(1, MAX_LIMIT).to_string())];
        if let Some(c) = after {
            query.push(("ascending", "true".to_owned()));
            query.push(("after", c.to_owned()));
        }
        if let Some(p) = &page {
            query.push(("page_token", p.page_token.clone()));
        }
        let listed = self.list_page(&query).await?;
        let mut raw = listed.messages;
        if cursor.is_none() {
            raw.reverse(); // newest-first from the provider, oldest-first for us
        }
        let newest = page
            .as_ref()
            .map(|p| p.newest.as_str())
            .into_iter()
            .chain(raw.iter().filter_map(|m| m.timestamp.as_deref()))
            .filter(|t| valid_cursor(t))
            .filter_map(|t| epoch(t).map(|e| (e, t)))
            .max_by_key(|(e, _)| *e)
            .map(|(_, t)| t.to_owned())
            .or_else(|| after.map(str::to_owned));
        let next_cursor = if let (Some(after), Some(token)) = (after, listed.next_page_token) {
            if !valid_page_token(&token) {
                return Err(ProviderError::BadResponse.into());
            }
            let continuation = PageCursor {
                after: after.to_owned(),
                newest: newest.clone().unwrap_or_else(|| after.to_owned()),
                page_token: token,
            };
            Some(format!(
                "{PAGE_CURSOR_PREFIX}{}",
                serde_json::to_string(&continuation).map_err(|_| ProviderError::BadResponse)?
            ))
        } else {
            newest
        };
        let own = self.address();
        let messages = raw
            .iter()
            .filter(|m| m.is_inbound(own))
            .map(MessageWire::summary)
            .collect();
        Ok(NewMessages {
            messages,
            next_cursor,
        })
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, EmailError> {
        Err(EmailError::unsupported("folders"))
    }

    async fn search(&self, req: &SearchRequest<'_>) -> Result<Found, EmailError> {
        let q = req.query;
        let mut query = vec![("limit", req.limit.clamp(1, MAX_LIMIT).to_string())];
        for (key, val) in [("from", &q.from), ("subject", &q.subject)] {
            if let Some(v) = val.as_deref().filter(|v| !v.is_empty()) {
                if v.chars().any(char::is_control) {
                    return Err(EmailError::Input(
                        "search text must not contain control characters".into(),
                    ));
                }
                query.push((key, v.to_owned()));
            }
        }
        if let Some(d) = &q.since {
            query.push(("after", day_start(d)?));
        }
        if let Some(d) = &q.before {
            query.push(("before", day_start(d)?));
        }
        match q.unread {
            Some(true) => query.push(("labels", "unread".to_owned())),
            Some(false) => {
                return Err(EmailError::unsupported("searching for read-only messages"));
            }
            None => {}
        }
        let raw = self.list(&query).await?;
        let messages: Vec<_> = raw.iter().map(MessageWire::summary).collect();
        let total = messages.len();
        Ok(Found { messages, total })
    }

    async fn get_message(
        &self,
        _folder: &str,
        id: &str,
        max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        check_id(id, "message id").map_err(bad_id)?;
        let inbox = self.inbox_id()?;
        let v = self
            .call(
                Method::GET,
                &["v0", "inboxes", inbox, "messages", id],
                &[],
                None,
                &[],
            )
            .await?;
        parse::<MessageWire>(v)?.detail(max_body_chars)
    }

    async fn mark_read(&self, _folder: &str, id: &str, read: bool) -> Result<(), EmailError> {
        check_id(id, "message id").map_err(bad_id)?;
        let inbox = self.inbox_id()?;
        let (add, remove) = if read {
            ("read", "unread")
        } else {
            ("unread", "read")
        };
        let body = json!({ "add_labels": [add], "remove_labels": [remove] });
        self.call(
            Method::PATCH,
            &["v0", "inboxes", inbox, "messages", id],
            &[],
            Some(&body),
            &[],
        )
        .await
        .map(|_| ())
    }

    async fn get_attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
        max_bytes: usize,
    ) -> Result<AttachmentData, EmailError> {
        check_id(message_id, "message id").map_err(bad_id)?;
        check_id(attachment_id, "attachment id").map_err(bad_id)?;
        let inbox = self.inbox_id()?;
        let v = self
            .call(
                Method::GET,
                &[
                    "v0",
                    "inboxes",
                    inbox,
                    "messages",
                    message_id,
                    "attachments",
                    attachment_id,
                ],
                &[],
                None,
                &[],
            )
            .await?;
        let meta: AttachmentMetaWire = parse(v)?;
        if meta
            .size
            .is_some_and(|s| usize::try_from(s).map_or(true, |s| s > max_bytes))
        {
            return Err(ProviderError::BadResponse.into());
        }
        let bytes = self.client.download(&meta.download_url, max_bytes).await?;
        Ok(AttachmentData {
            bytes,
            content_type: meta.content_type.as_deref().map(cap_untrusted),
            filename: meta.filename.as_deref().map(cap_untrusted),
        })
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<Receipt, EmailError> {
        let inbox = self.inbox_id()?;
        self.check_outgoing(mail, true)?;
        self.post_send(
            &["v0", "inboxes", inbox, "messages", "send"],
            &send_body(mail),
        )
        .await
    }

    async fn reply(
        &self,
        message_id: &str,
        mail: &OutgoingMail,
        reply_all: bool,
    ) -> Result<Receipt, EmailError> {
        check_id(message_id, "message id").map_err(bad_id)?;
        let inbox = self.inbox_id()?;
        self.check_outgoing(mail, false)?;
        self.post_send(
            &["v0", "inboxes", inbox, "messages", message_id, "reply"],
            &reply_body(mail, reply_all),
        )
        .await
    }
}
