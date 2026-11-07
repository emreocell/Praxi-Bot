//! IMAP access (implicit TLS only) built on `async-imap`.

use std::fmt::Write as _;

use async_imap::types::Flag;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::config::AccountConfig;
use crate::error::EmailError;
use crate::message::{MessageDetail, MessageSummary, imap_date, parse_detail, parse_summary};
use crate::tls;

/// Default number of search results.
pub const DEFAULT_LIMIT: usize = 20;
/// Hard cap on search results.
pub const MAX_LIMIT: usize = 100;

fn imap_err(e: impl std::fmt::Display) -> EmailError {
    EmailError::Imap(e.to_string())
}

/// A mailbox folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderInfo {
    /// Folder name as the server reports it.
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
        for (key, val) in [("FROM", &self.from), ("SUBJECT", &self.subject)] {
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

type Stream = TlsStream<TcpStream>;

/// An authenticated IMAP session.
pub struct ImapSession {
    session: async_imap::Session<Stream>,
}

impl ImapSession {
    /// Connect with implicit TLS and log in.
    ///
    /// # Errors
    /// TLS, network or authentication failures.
    pub async fn connect(acct: &AccountConfig, password: &str) -> Result<Self, EmailError> {
        let connector = tls::connector(acct.extra_root_pem.as_deref())?;
        let host = acct.imap.host.clone();
        let tcp = TcpStream::connect((host.as_str(), acct.imap.port))
            .await
            .map_err(|e| imap_err(format!("connect {host}:{}: {e}", acct.imap.port)))?;
        let name = ServerName::try_from(host.clone())
            .map_err(|e| EmailError::Tls(format!("invalid server name {host}: {e}")))?;
        let stream = connector
            .connect(name, tcp)
            .await
            .map_err(|e| EmailError::Tls(format!("handshake with {host}: {e}")))?;
        let mut client = async_imap::Client::new(stream);
        client
            .read_response()
            .await
            .map_err(imap_err)?
            .ok_or_else(|| imap_err("server closed the connection before greeting"))?;
        let session = client
            .login(acct.login(), password)
            .await
            .map_err(|_| imap_err("login failed"))?;
        Ok(Self { session })
    }

    /// List all folders.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn list_folders(&mut self) -> Result<Vec<FolderInfo>, EmailError> {
        let names: Vec<_> = self
            .session
            .list(Some(""), Some("*"))
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        Ok(names
            .iter()
            .map(|n| FolderInfo {
                name: n.name().to_string(),
                delimiter: n.delimiter().map(str::to_string),
                attributes: n.attributes().iter().map(|a| format!("{a:?}")).collect(),
            })
            .collect())
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
        check_folder(folder)?;
        let criteria = query.to_imap()?;
        self.session.examine(folder).await.map_err(imap_err)?;
        let mut uids: Vec<u32> = self
            .session
            .uid_search(&criteria)
            .await
            .map_err(imap_err)?
            .into_iter()
            .collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        let total = uids.len();
        uids.truncate(limit.clamp(1, MAX_LIMIT));
        if uids.is_empty() {
            return Ok((Vec::new(), total));
        }
        let mut set = String::new();
        for (i, u) in uids.iter().enumerate() {
            if i > 0 {
                set.push(',');
            }
            let _ = write!(set, "{u}");
        }
        let fetches: Vec<_> = self
            .session
            .uid_fetch(&set, "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])")
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        let mut out: Vec<MessageSummary> = fetches
            .iter()
            .filter_map(|f| {
                let uid = f.uid?;
                let unread = !f.flags().any(|fl| matches!(fl, Flag::Seen));
                Some(parse_summary(uid, f.header()?, unread, f.size))
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
        check_folder(folder)?;
        self.session.examine(folder).await.map_err(imap_err)?;
        let fetches: Vec<_> = self
            .session
            .uid_fetch(uid.to_string(), "(UID FLAGS RFC822.SIZE BODY.PEEK[])")
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        let f = fetches
            .iter()
            .find(|f| f.uid == Some(uid) && f.body().is_some())
            .ok_or_else(|| EmailError::NotFound(format!("message uid {uid} in {folder}")))?;
        let unread = !f.flags().any(|fl| matches!(fl, Flag::Seen));
        parse_detail(
            uid,
            f.body().unwrap_or_default(),
            unread,
            f.size,
            max_body_chars,
        )
    }

    /// Set or clear `\Seen` on a message.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn set_seen(&mut self, folder: &str, uid: u32, seen: bool) -> Result<(), EmailError> {
        check_folder(folder)?;
        self.session.select(folder).await.map_err(imap_err)?;
        let op = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let _: Vec<_> = self
            .session
            .uid_store(uid.to_string(), op)
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        Ok(())
    }

    /// Append a raw RFC 5322 message to `folder` flagged `\Draft`.
    ///
    /// # Errors
    /// IMAP failures.
    pub async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<(), EmailError> {
        check_folder(folder)?;
        self.session
            .append(folder, Some("(\\Draft \\Seen)"), None, raw)
            .await
            .map_err(imap_err)
    }

    /// Log out, ignoring errors.
    pub async fn logout(mut self) {
        let _ = self.session.logout().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
