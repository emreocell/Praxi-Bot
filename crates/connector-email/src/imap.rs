//! IMAP access (implicit TLS only) built on `async-imap`.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_imap::error::Error as ImapError;
use async_imap::types::Flag;
use base64::Engine as _;
use base64::alphabet::Alphabet;
use base64::engine::general_purpose::{GeneralPurpose, NO_PAD};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::config::AccountConfig;
use crate::error::EmailError;
use crate::message::{
    MAX_BODY_CHARS, MAX_MESSAGE_BYTES, MessageDetail, MessageSummary, cap_untrusted, imap_date,
    over_message_cap, parse_detail, parse_summary,
};
use crate::tls;

/// `async-imap` logs every encoded command, including `LOGIN` and the password,
/// at trace on this target (`imap_stream.rs`).
const IMAP_STREAM_TARGET: &str = "async_imap::imap_stream";

static IMAP_TRACE_HOLDS: AtomicUsize = AtomicUsize::new(0);
static IMAP_TRACE_LOWERED: AtomicBool = AtomicBool::new(false);
static IMAP_TRACE_SAVED: AtomicUsize = AtomicUsize::new(0);

fn trace_is_blocked(target: &str, level: log::Level) -> bool {
    level == log::Level::Trace && target.starts_with(IMAP_STREAM_TARGET)
}

fn level_from_usize(value: usize) -> log::LevelFilter {
    match value {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    }
}

/// While an IMAP session exists, keep `async_imap::imap_stream` off at trace.
/// The `log` facade has no per-target runtime filter, so the ceiling is debug
/// for the whole process until the last session ends.
struct ImapTraceGuard;

impl ImapTraceGuard {
    fn enter() -> Self {
        let prev = IMAP_TRACE_HOLDS.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            enforce_imap_trace_ceiling();
        }
        Self
    }
}

impl Drop for ImapTraceGuard {
    fn drop(&mut self) {
        let left = IMAP_TRACE_HOLDS.fetch_sub(1, Ordering::SeqCst);
        if left == 1 && IMAP_TRACE_LOWERED.swap(false, Ordering::SeqCst) {
            log::set_max_level(level_from_usize(IMAP_TRACE_SAVED.load(Ordering::SeqCst)));
        }
    }
}

fn enforce_imap_trace_ceiling() {
    if log::max_level() == log::LevelFilter::Trace
        && trace_is_blocked(IMAP_STREAM_TARGET, log::Level::Trace)
    {
        if !IMAP_TRACE_LOWERED.swap(true, Ordering::SeqCst) {
            IMAP_TRACE_SAVED.store(log::LevelFilter::Trace as usize, Ordering::SeqCst);
        }
        log::set_max_level(log::LevelFilter::Debug);
    }
}

fn redact(text: &str, password: &str) -> String {
    if password.is_empty() {
        text.to_string()
    } else {
        text.replace(password, "[REDACTED]")
    }
}

/// Fixed IMAP failures. Server `NO` / `BAD` text is dropped, then the password
/// is stripped in case it appears in the operation label.
fn fixed_imap_error(op: &str, err: &ImapError, password: &str) -> EmailError {
    let kind = match err {
        ImapError::No(_) => "rejected",
        ImapError::Bad(_) => "bad response",
        ImapError::Io(_) => "io error",
        ImapError::ConnectionLost => "connection lost",
        ImapError::Parse(_) => "parse error",
        ImapError::Validate(_) => "invalid command",
        ImapError::Append => "append failed",
        _ => "failed",
    };
    EmailError::Imap(redact(&format!("{op} {kind}"), password))
}

/// Default number of search results.
pub const DEFAULT_LIMIT: usize = 20;
/// Hard cap on search results.
pub const MAX_LIMIT: usize = 100;

/// A mailbox folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderInfo {
    /// Decoded and capped display name, not necessarily a round-trip selector.
    /// For example, a mailbox whose wire name is `Entw&-APw-rfe` displays as
    /// `Entw&APw-rfe`, which as an input selects `Entwürfe` instead.
    pub name: String,
    /// Hierarchy delimiter.
    pub delimiter: Option<String>,
    /// Server attributes (e.g. `\Drafts`).
    pub attributes: Vec<String>,
}

/// Search criteria; messages must match every supplied criterion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    /// Sender substring.
    pub from: Option<String>,
    /// Subject substring.
    pub subject: Option<String>,
    /// Received on/after, `YYYY-MM-DD`.
    pub since: Option<String>,
    /// Received before, `YYYY-MM-DD`.
    pub before: Option<String>,
    /// `true` = unread only, `false` = read only.
    pub unread: Option<bool>,
    /// Text that must occur in the message body (IMAP `BODY`).
    pub text: Option<String>,
    /// API mailboxes hide spam and unauthenticated mail unless asked; `true`
    /// includes it. IMAP accounts have the spam folder to name instead.
    pub include_junk: Option<bool>,
}

/// Quote a string as an IMAP quoted string.
fn quote(s: &str) -> Result<String, EmailError> {
    if s.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err(EmailError::Input(
            "search text must not contain line breaks or NUL".into(),
        ));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

impl SearchQuery {
    /// Render the IMAP `SEARCH` argument list.
    ///
    /// # Errors
    /// [`EmailError::Input`] for bad dates or text with control characters.
    pub fn to_imap(&self) -> Result<String, EmailError> {
        let mut parts: Vec<String> = Vec::new();
        let mut non_ascii = false;
        for (key, val) in [
            ("FROM", &self.from),
            ("SUBJECT", &self.subject),
            ("BODY", &self.text),
        ] {
            if let Some(v) = val.as_deref().filter(|v| !v.is_empty()) {
                non_ascii |= !v.is_ascii();
                parts.push(format!("{key} {}", quote(v)?));
            }
        }
        if let Some(d) = &self.since {
            parts.push(format!("SINCE {}", imap_date(d)?));
        }
        if let Some(d) = &self.before {
            parts.push(format!("BEFORE {}", imap_date(d)?));
        }
        match self.unread {
            Some(true) => parts.push("UNSEEN".into()),
            Some(false) => parts.push("SEEN".into()),
            None => {}
        }
        let mut q = String::new();
        if non_ascii {
            q.push_str("CHARSET UTF-8 ");
        }
        if parts.is_empty() {
            q.push_str("ALL");
        } else {
            q.push_str(&parts.join(" "));
        }
        Ok(q)
    }
}

pub(crate) fn check_folder(name: &str) -> Result<(), EmailError> {
    if name.is_empty() || name.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err(EmailError::Input("invalid folder name".into()));
    }
    Ok(())
}

// RFC 3501 §5.1.3: UTF-16BE in unpadded Base64, with ',' instead of '/'.
const MODIFIED_BASE64: GeneralPurpose = {
    let Ok(alphabet) =
        Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,")
    else {
        panic!("invalid IMAP Base64 alphabet")
    };
    GeneralPurpose::new(&alphabet, NO_PAD)
};

fn direct(c: char) -> bool {
    (' '..='~').contains(&c)
}

pub(crate) fn encode_mailbox_name(name: &str) -> String {
    fn flush_shift(out: &mut String, bytes: &mut Vec<u8>) {
        if !bytes.is_empty() {
            out.push('&');
            MODIFIED_BASE64.encode_string(&*bytes, out);
            out.push('-');
            bytes.clear();
        }
    }

    let mut out = String::with_capacity(name.len());
    let mut bytes = Vec::new();
    for c in name.chars() {
        if direct(c) {
            flush_shift(&mut out, &mut bytes);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                bytes.extend_from_slice(&unit.to_be_bytes());
            }
        }
    }
    flush_shift(&mut out, &mut bytes);
    out
}

pub(crate) fn decode_mailbox_name(mut name: &str) -> Result<String, EmailError> {
    let invalid = || EmailError::Input("invalid modified UTF-7 folder name".into());
    let mut out = String::with_capacity(name.len());
    let mut previous_shift = false;
    while let Some((plain, shifted)) = name.split_once('&') {
        if !plain.chars().all(direct) {
            return Err(invalid());
        }
        out.push_str(plain);
        let (encoded, rest) = shifted.split_once('-').ok_or_else(invalid)?;
        if encoded.is_empty() {
            out.push('&');
            previous_shift = false;
        } else {
            // Adjacent non-empty shifts have no intervening direct character.
            if previous_shift && plain.is_empty() {
                return Err(invalid());
            }
            let bytes = MODIFIED_BASE64.decode(encoded).map_err(|_| invalid())?;
            let (pairs, remainder) = bytes.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(invalid());
            }
            let units = pairs.iter().copied().map(u16::from_be_bytes);
            for c in char::decode_utf16(units) {
                let c = c.map_err(|_| invalid())?;
                if direct(c) {
                    return Err(invalid());
                }
                out.push(c);
            }
            previous_shift = true;
        }
        name = rest;
    }
    if !name.chars().all(direct) {
        return Err(invalid());
    }
    out.push_str(name);
    Ok(out)
}

fn wire_mailbox_name(name: &str) -> Result<Cow<'_, str>, EmailError> {
    check_folder(name)?;
    // Keep legacy ASCII (including already-encoded names) byte-for-byte.
    // If any shift is invalid, interpret the entire input as a literal name.
    if name.is_ascii() && (!name.contains('&') || decode_mailbox_name(name).is_ok()) {
        Ok(Cow::Borrowed(name))
    } else {
        Ok(Cow::Owned(encode_mailbox_name(name)))
    }
}

fn display_mailbox_name(name: &str) -> Option<String> {
    // A mailbox name is also a SELECT identifier. Keep permitted characters
    // (including tabs) so displaying a folder cannot select a different one.
    // Sender/subject display text still uses the stricter header cleaner.
    let cap_name = |value: &str| {
        value
            .chars()
            .take(crate::message::MAX_UNTRUSTED_FIELD_CHARS)
            .collect()
    };
    if name.is_ascii() {
        // Like outgoing selectors, keep permitted legacy ASCII names with
        // no shifts byte-for-byte (subject to the display cap).
        if !name.contains('&') {
            check_folder(name).ok()?;
            return Some(cap_name(name));
        }
        // Omit a malformed name rather than returning text that a caller
        // could pass back and inadvertently select a different mailbox.
        decode_mailbox_name(name).ok().map(|name| cap_name(&name))
    } else {
        // RFC 3501 §5.1 recommends interpreting 8-bit names as UTF-8.
        // This is already a string; preserve any literal ampersands.
        Some(cap_name(name))
    }
}

type Stream = TlsStream<TcpStream>;

/// Where [`ImapSession::connect_staged`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectStage {
    Connect,
    Tls,
    Greeting,
    Login,
}

/// Most bytes a message may have when an attachment of up to `max_bytes` is
/// downloaded from it: a message with a big attachment is bigger than the
/// mail-reading cap, so the attachment's own cap comes on top of it. The
/// attachment travels base64 encoded (4/3 of its size, plus line breaks), so half
/// as much again is allowed.
pub(crate) fn attachment_fetch_cap(max_bytes: usize) -> u64 {
    u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(u64::try_from(max_bytes / 2).unwrap_or(u64::MAX))
        .saturating_add(u64::from(MAX_MESSAGE_BYTES))
}

/// An authenticated IMAP session.
pub struct ImapSession {
    session: async_imap::Session<Stream>,
    password: String,
    /// UIDVALIDITY the server reported when the last search selected its folder.
    uid_validity: Option<u32>,
    _trace: ImapTraceGuard,
}

impl ImapSession {
    /// Connect with implicit TLS and log in.
    ///
    /// # Errors
    /// TLS, network or authentication failures.
    pub async fn connect(acct: &AccountConfig, password: &str) -> Result<Self, EmailError> {
        Self::connect_inner(acct, password)
            .await
            .map_err(|(_, e)| e)
    }

    /// [`Self::connect`] that only says which stage failed (for the
    /// "test connection" report, which must not carry server text).
    pub(crate) async fn connect_staged(
        acct: &AccountConfig,
        password: &str,
    ) -> Result<Self, ConnectStage> {
        Self::connect_inner(acct, password)
            .await
            .map_err(|(stage, _)| stage)
    }

    async fn connect_inner(
        acct: &AccountConfig,
        password: &str,
    ) -> Result<Self, (ConnectStage, EmailError)> {
        let trace = ImapTraceGuard::enter();
        enforce_imap_trace_ceiling();
        let tls_err = |e: EmailError| (ConnectStage::Tls, e);
        let connector = tls::connector(acct.extra_root_pem.as_deref()).map_err(tls_err)?;
        let host = acct.imap.host.clone();
        let tcp = TcpStream::connect((host.as_str(), acct.imap.port))
            .await
            .map_err(|e| {
                (
                    ConnectStage::Connect,
                    EmailError::Imap(redact(
                        &format!("connect {host}:{}: {e}", acct.imap.port),
                        password,
                    )),
                )
            })?;
        let name = ServerName::try_from(host.clone())
            .map_err(|e| tls_err(EmailError::Tls(format!("invalid server name {host}: {e}"))))?;
        let stream = connector
            .connect(name, tcp)
            .await
            .map_err(|e| tls_err(EmailError::Tls(format!("handshake with {host}: {e}"))))?;
        let greeting = || {
            (
                ConnectStage::Greeting,
                EmailError::Imap(redact("greeting failed", password)),
            )
        };
        let mut client = async_imap::Client::new(stream);
        client
            .read_response()
            .await
            .map_err(|_| greeting())?
            .ok_or_else(greeting)?;
        let session = client.login(acct.login(), password).await.map_err(|_| {
            (
                ConnectStage::Login,
                EmailError::Imap(redact("login failed", password)),
            )
        })?;
        Ok(Self {
            uid_validity: None,
            session,
            password: password.to_owned(),
            _trace: trace,
        })
    }

    fn fail(&self, op: &str, err: &ImapError) -> EmailError {
        fixed_imap_error(op, err, &self.password)
    }

    async fn uid_fetch(
        &mut self,
        set: &str,
        query: &str,
    ) -> Result<Vec<async_imap::types::Fetch>, EmailError> {
        enforce_imap_trace_ceiling();
        let password = self.password.clone();
        let stream = self
            .session
            .uid_fetch(set, query)
            .await
            .map_err(|e| fixed_imap_error("fetch", &e, &password))?;
        stream
            .try_collect()
            .await
            .map_err(|e| fixed_imap_error("fetch", &e, &password))
    }

    /// List folders with decoded display names, omitting malformed names.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn list_folders(&mut self) -> Result<Vec<FolderInfo>, EmailError> {
        enforce_imap_trace_ceiling();
        let password = self.password.clone();
        let stream = self
            .session
            .list(Some(""), Some("*"))
            .await
            .map_err(|e| fixed_imap_error("list", &e, &password))?;
        let names: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| fixed_imap_error("list", &e, &password))?;
        Ok(names
            .iter()
            .filter_map(|n| {
                Some(FolderInfo {
                    name: display_mailbox_name(n.name())?,
                    delimiter: n.delimiter().map(cap_untrusted),
                    attributes: n
                        .attributes()
                        .iter()
                        .map(|a| cap_untrusted(&format!("{a:?}")))
                        .collect(),
                })
            })
            .collect())
    }

    /// UIDVALIDITY of the folder the last [`Self::search`] selected, when the
    /// server reported one. UIDs are only comparable while it is unchanged.
    #[must_use]
    pub fn uid_validity(&self) -> Option<u32> {
        self.uid_validity
    }

    /// Search a folder read-only; newest (highest UID) first. Returns the
    /// summaries and the total number of matches.
    ///
    /// # Errors
    /// Bad input or IMAP failures.
    pub async fn search(
        &mut self,
        folder: &str,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<(Vec<MessageSummary>, usize), EmailError> {
        let wire_name = wire_mailbox_name(folder)?;
        let criteria = query.to_imap()?;
        enforce_imap_trace_ceiling();
        let mailbox = self
            .session
            .examine(wire_name)
            .await
            .map_err(|e| self.fail("examine", &e))?;
        self.uid_validity = mailbox.uid_validity;
        let mut uids: Vec<u32> = self
            .session
            .uid_search(&criteria)
            .await
            .map_err(|e| self.fail("search", &e))?
            .into_iter()
            .collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        let total = uids.len();
        uids.truncate(limit.clamp(1, MAX_LIMIT));
        if uids.is_empty() {
            return Ok((Vec::new(), total));
        }
        let set = uid_set(&uids);
        let sized = self.uid_fetch(&set, "(UID RFC822.SIZE)").await?;
        let allowed: Vec<u32> = sized
            .iter()
            .filter_map(|f| {
                let uid = f.uid?;
                let size = f.size?;
                (size <= MAX_MESSAGE_BYTES).then_some(uid)
            })
            .collect();
        if allowed.is_empty() {
            return Ok((Vec::new(), total));
        }
        let header_set = uid_set(&allowed);
        let fetches = self
            .uid_fetch(
                &header_set,
                "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER] BODYSTRUCTURE)",
            )
            .await?;
        let mut out: Vec<MessageSummary> = fetches
            .iter()
            .filter_map(|f| {
                let uid = f.uid?;
                let size = f.size?;
                if size > MAX_MESSAGE_BYTES {
                    return None;
                }
                let header = f.header()?;
                if over_message_cap(header.len()) {
                    return None;
                }
                let unread = !f.flags().any(|fl| matches!(fl, Flag::Seen));
                let mut summary = parse_summary(uid, header, unread, f.size);
                summary.has_attachment = f.bodystructure().map(|body| has_attachment(body, 0));
                Some(summary)
            })
            .collect();
        out.sort_unstable_by_key(|a| std::cmp::Reverse(a.uid));
        Ok((out, total))
    }

    /// Fetch one message without changing its `\Seen` flag.
    ///
    /// # Errors
    /// [`EmailError::NotFound`] if the UID does not exist; IMAP failures.
    pub async fn fetch(
        &mut self,
        folder: &str,
        uid: u32,
        max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        let wire_name = wire_mailbox_name(folder)?;
        enforce_imap_trace_ceiling();
        let max_body_chars = max_body_chars.min(MAX_BODY_CHARS);
        self.session
            .examine(wire_name)
            .await
            .map_err(|e| self.fail("examine", &e))?;
        let sized = self
            .uid_fetch(&uid.to_string(), "(UID RFC822.SIZE)")
            .await?;
        let meta = sized.iter().find(|f| f.uid == Some(uid));
        let Some(meta) = meta else {
            return Err(EmailError::NotFound(format!(
                "message uid {uid} in {folder}"
            )));
        };
        let Some(size) = meta.size else {
            return Err(EmailError::Imap(format!(
                "message uid {uid} has no size; refused"
            )));
        };
        if size > MAX_MESSAGE_BYTES {
            return Err(EmailError::Imap(format!(
                "message uid {uid} is {size} bytes, over the {MAX_MESSAGE_BYTES} byte cap"
            )));
        }
        let fetches = self
            .uid_fetch(&uid.to_string(), "(UID FLAGS RFC822.SIZE BODY.PEEK[])")
            .await?;
        let f = fetches
            .iter()
            .find(|f| f.uid == Some(uid) && f.body().is_some())
            .ok_or_else(|| EmailError::NotFound(format!("message uid {uid} in {folder}")))?;
        let body = f.body().unwrap_or_default();
        if over_message_cap(body.len()) {
            return Err(EmailError::Imap(format!(
                "message uid {uid} is {} bytes, over the {MAX_MESSAGE_BYTES} byte cap",
                body.len()
            )));
        }
        let unread = !f.flags().any(|fl| matches!(fl, Flag::Seen));
        parse_detail(uid, body, unread, f.size, max_body_chars)
    }

    /// Download one attachment of a message (by its index among the message's
    /// attachments, as listed by `fetch`), refusing more than `max_bytes`.
    ///
    /// # Errors
    /// [`EmailError::NotFound`] for an unknown message or attachment; IMAP failures.
    pub async fn fetch_attachment(
        &mut self,
        folder: &str,
        uid: u32,
        index: usize,
        max_bytes: usize,
    ) -> Result<crate::provider::AttachmentData, EmailError> {
        let wire_name = wire_mailbox_name(folder)?;
        enforce_imap_trace_ceiling();
        self.session
            .examine(wire_name)
            .await
            .map_err(|e| self.fail("examine", &e))?;
        // Ask the size first so nothing larger is ever downloaded.
        let allowed = attachment_fetch_cap(max_bytes);
        let sized = self
            .uid_fetch(&uid.to_string(), "(UID RFC822.SIZE)")
            .await?;
        let size = sized
            .iter()
            .find(|f| f.uid == Some(uid))
            .ok_or_else(|| EmailError::NotFound(format!("message uid {uid} in {folder}")))?
            .size
            .ok_or_else(|| EmailError::Imap(format!("message uid {uid} has no size; refused")))?;
        if u64::from(size) > allowed {
            return Err(EmailError::Imap(format!(
                "message uid {uid} is {size} bytes, too large to download an attachment from"
            )));
        }
        let fetches = self
            .uid_fetch(&uid.to_string(), "(UID RFC822.SIZE BODY.PEEK[])")
            .await?;
        let f = fetches
            .iter()
            .find(|f| f.uid == Some(uid) && f.body().is_some())
            .ok_or_else(|| EmailError::NotFound(format!("message uid {uid} in {folder}")))?;
        let body = f.body().unwrap_or_default();
        // The size above is what the server said, not what it sent: a server that
        // lies must not get its bytes parsed (and held) all the same.
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > allowed {
            return Err(EmailError::Imap(format!(
                "message uid {uid} is {} bytes, too large to download an attachment from",
                body.len()
            )));
        }
        crate::message::attachment_of(body, index, max_bytes)
    }

    /// Set or clear `\Seen` on a message.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn set_seen(&mut self, folder: &str, uid: u32, seen: bool) -> Result<(), EmailError> {
        let wire_name = wire_mailbox_name(folder)?;
        enforce_imap_trace_ceiling();
        self.session
            .select(wire_name)
            .await
            .map_err(|e| self.fail("select", &e))?;
        let op = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let password = self.password.clone();
        let stream = self
            .session
            .uid_store(uid.to_string(), op)
            .await
            .map_err(|e| fixed_imap_error("store", &e, &password))?;
        let _: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| fixed_imap_error("store", &e, &password))?;
        Ok(())
    }

    /// Append a raw RFC 5322 message to `folder` flagged `\Draft`.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<(), EmailError> {
        let wire_name = wire_mailbox_name(folder)?;
        enforce_imap_trace_ceiling();
        self.session
            .append(wire_name, Some("(\\Draft \\Seen)"), None, raw)
            .await
            .map_err(|e| self.fail("append", &e))
    }

    /// Log out, ignoring errors.
    pub async fn logout(mut self) {
        enforce_imap_trace_ceiling();
        let _ = self.session.logout().await;
    }
}

fn uid_set(uids: &[u32]) -> String {
    let mut set = String::new();
    for (i, u) in uids.iter().enumerate() {
        if i > 0 {
            set.push(',');
        }
        let _ = write!(set, "{u}");
    }
    set
}

// BODYSTRUCTURE is metadata; never fetch a body to decide an attachment filter.
fn has_attachment(body: &async_imap::imap_proto::BodyStructure<'_>, depth: usize) -> bool {
    use async_imap::imap_proto::BodyStructure;
    if depth > 16 {
        return false;
    }
    let (common, children) = match body {
        BodyStructure::Basic { common, .. } | BodyStructure::Text { common, .. } => (common, false),
        BodyStructure::Message { common, body, .. } => (common, has_attachment(body, depth + 1)),
        BodyStructure::Multipart { common, bodies, .. } => {
            (common, bodies.iter().any(|b| has_attachment(b, depth + 1)))
        }
    };
    children
        || common
            .disposition
            .as_ref()
            .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_utf7_known_answers() {
        for (plain, wire) in [
            ("INBOX", "INBOX"),
            ("Drafts", "Drafts"),
            ("R&D", "R&-D"),
            ("Entwürfe", "Entw&APw-rfe"),
            ("~peter/mail/台北/日本語", "~peter/mail/&U,BTFw-/&ZeVnLIqe-"),
            ("😀", "&2D3eAA-"),
            ("☺!", "&Jjo-!"),
        ] {
            assert_eq!(encode_mailbox_name(plain), wire);
            assert_eq!(decode_mailbox_name(wire).unwrap(), plain);
        }
    }

    #[test]
    fn modified_utf7_rejects_invalid_wire_names() {
        for malformed in [
            "&APw",               // valid payload with missing shift terminator
            "R&D",                // missing shift terminator
            "&!-",                // not modified Base64
            "&APw=-",             // padding
            "&U/BTFw-",           // ordinary Base64 slash
            "&APx-",              // non-zero trailing bits
            "&QQ-",               // odd UTF-16BE byte count
            "&2AA-",              // unpaired surrogate
            "&AEE-",              // shifted printable ASCII
            "&U,BTFw-&ZeVnLIqe-", // superfluous shift (RFC example)
            "Entwürfe",           // direct non-ASCII
            "name\t&APw-",        // direct control before a valid shift
            "name\t",             // direct control character
        ] {
            assert!(
                matches!(decode_mailbox_name(malformed), Err(EmailError::Input(_))),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn wire_names_preserve_only_whole_valid_encodings() {
        for (input, expected) in [
            ("Drafts", "Drafts"),
            ("INBOX", "INBOX"),
            ("name\t", "name\t"),
            ("Entwürfe", "Entw&APw-rfe"),
            ("Entw&APw-rfe", "Entw&APw-rfe"),
            ("Entw&-APw-rfe", "Entw&-APw-rfe"),
            ("Entw&APw-rfe/R&D", "Entw&-APw-rfe/R&-D"),
        ] {
            assert_eq!(wire_mailbox_name(input).unwrap(), expected);
        }
        for invalid in ["", "name\r", "name\n", "name\0"] {
            assert!(matches!(
                wire_mailbox_name(invalid),
                Err(EmailError::Input(_))
            ));
        }
    }

    #[test]
    fn builds_search() {
        let q = SearchQuery {
            from: Some("ann@x.com".into()),
            subject: Some("say \"hi\"".into()),
            since: Some("2026-01-02".into()),
            unread: Some(true),
            ..Default::default()
        };
        assert_eq!(
            q.to_imap().unwrap(),
            "FROM \"ann@x.com\" SUBJECT \"say \\\"hi\\\"\" SINCE 2-Jan-2026 UNSEEN"
        );
        assert_eq!(SearchQuery::default().to_imap().unwrap(), "ALL");
    }

    #[test]
    fn non_ascii_uses_charset() {
        let q = SearchQuery {
            subject: Some("fatura \u{15f}".into()),
            ..Default::default()
        };
        assert!(q.to_imap().unwrap().starts_with("CHARSET UTF-8 SUBJECT"));
    }

    #[test]
    fn rejects_injection() {
        let q = SearchQuery {
            from: Some("a\r\nA2 DELETE INBOX".into()),
            ..Default::default()
        };
        assert!(q.to_imap().is_err());
        assert!(check_folder("INBOX\r\nX").is_err());
    }

    #[test]
    fn imap_stream_trace_target_is_suppressed() {
        assert!(trace_is_blocked(IMAP_STREAM_TARGET, log::Level::Trace));
        assert!(!trace_is_blocked(IMAP_STREAM_TARGET, log::Level::Debug));
        assert!(!trace_is_blocked(
            "connector_email::imap",
            log::Level::Trace
        ));
    }

    #[test]
    fn fixed_imap_error_drops_server_text_and_password() {
        let err = ImapError::No("NO marker-secret-echo test-app-password".into());
        let text = fixed_imap_error("examine", &err, "test-app-password").to_string();
        assert!(!text.contains("test-app-password"), "{text}");
        assert!(!text.contains("marker-secret-echo"), "{text}");
        assert!(text.contains("examine rejected"), "{text}");
    }
}
