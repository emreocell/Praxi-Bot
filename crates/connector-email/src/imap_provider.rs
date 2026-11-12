//! [`ImapSmtpProvider`]: the user's own mailbox over IMAP (read) and SMTP
//! (send), as one [`MailboxProvider`]. It has no `create_inbox`, no push and no
//! thread reply.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use secret_store::SecretStore;

use crate::config::{AccountConfig, AccountKind};
use crate::cred::resolve_secret;
use crate::error::EmailError;
use crate::imap::{FolderInfo, ImapSession};
use crate::message::MessageDetail;
use crate::provider::{Capabilities, Found, MailboxProvider, Receipt, SearchRequest};
use crate::smtp::{self, OutgoingMail};

/// IMAP/SMTP access to one account. The password is resolved from the secret
/// store for every operation and never kept.
pub struct ImapSmtpProvider {
    acct: AccountConfig,
    secrets: Arc<dyn SecretStore>,
}

impl ImapSmtpProvider {
    /// Provider for `acct` (an [`AccountKind::ImapSmtp`] account).
    #[must_use]
    pub fn new(acct: AccountConfig, secrets: Arc<dyn SecretStore>) -> Self {
        Self { acct, secrets }
    }

    /// One deadline around secret resolution and the network call.
    pub(crate) async fn deadline<T>(
        limit: std::time::Duration,
        secs: u64,
        fut: impl Future<Output = Result<T, EmailError>>,
    ) -> Result<T, EmailError> {
        match tokio::time::timeout(limit, fut).await {
            Ok(result) => result,
            Err(_) => Err(EmailError::Timeout(secs)),
        }
    }

    async fn with_imap<T, F>(&self, f: F) -> Result<T, EmailError>
    where
        F: for<'a> FnOnce(
            &'a mut ImapSession,
            &'a AccountConfig,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, EmailError>> + Send + 'a>,
        >,
    {
        let limit = self.acct.timeout();
        let secs = limit.as_secs();
        Self::deadline(limit, secs, async {
            let pw = resolve_secret(&self.secrets, &self.acct.password).await?;
            let mut s = ImapSession::connect(&self.acct, &pw).await?;
            let r = f(&mut s, &self.acct).await;
            s.logout().await;
            r
        })
        .await
    }
}

fn parse_uid(id: &str) -> Result<u32, EmailError> {
    id.trim()
        .parse::<u32>()
        .ok()
        .filter(|u| *u != 0)
        .ok_or_else(|| EmailError::Input("`uid` (a positive number) is required".into()))
}

#[async_trait]
impl MailboxProvider for ImapSmtpProvider {
    fn kind(&self) -> AccountKind {
        AccountKind::ImapSmtp
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_inbox: false,
            delete_inbox: false,
            folders: true,
            search: true,
            mark_read: true,
            drafts: true,
            list_new: false,
            attachments: false,
            reply: false,
            push: false,
        }
    }

    async fn check(&self) -> Result<(), EmailError> {
        let pw = resolve_secret(&self.secrets, &self.acct.password).await?;
        let report = crate::probe::probe(&self.acct, &pw).await;
        if report.all_ok() {
            Ok(())
        } else {
            Err(EmailError::Imap("connection test failed".into()))
        }
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, EmailError> {
        self.with_imap(|s, _| Box::pin(s.list_folders())).await
    }

    async fn search(&self, req: &SearchRequest<'_>) -> Result<Found, EmailError> {
        req.query.to_imap()?; // validate before connecting
        crate::imap::check_folder(req.folder)?;
        let folder = req.folder.to_owned();
        let query = req.query.clone();
        let limit = req.limit;
        let (messages, total) = self
            .with_imap(move |s, _| Box::pin(async move { s.search(&folder, &query, limit).await }))
            .await?;
        Ok(Found { messages, total })
    }

    async fn get_message(
        &self,
        folder: &str,
        id: &str,
        max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        crate::imap::check_folder(folder)?;
        let uid = parse_uid(id)?;
        let folder = folder.to_owned();
        self.with_imap(move |s, _| {
            Box::pin(async move { s.fetch(&folder, uid, max_body_chars).await })
        })
        .await
    }

    async fn mark_read(&self, folder: &str, id: &str, read: bool) -> Result<(), EmailError> {
        crate::imap::check_folder(folder)?;
        let uid = parse_uid(id)?;
        let folder = folder.to_owned();
        self.with_imap(move |s, _| Box::pin(async move { s.set_seen(&folder, uid, read).await }))
            .await
    }

    async fn save_draft(&self, mail: &OutgoingMail) -> Result<String, EmailError> {
        let raw = smtp::compose_draft(&self.acct.address, mail)?.formatted();
        self.with_imap(|s, acct| {
            Box::pin(async move {
                s.append_draft(&acct.drafts_folder, &raw).await?;
                Ok(acct.drafts_folder.clone())
            })
        })
        .await
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<Receipt, EmailError> {
        let msg = smtp::compose(&self.acct.address, mail)?;
        let limit = self.acct.timeout();
        let secs = limit.as_secs();
        let response = Self::deadline(limit, secs, async {
            let pw = resolve_secret(&self.secrets, &self.acct.password).await?;
            smtp::send(&self.acct, &pw, msg).await
        })
        .await?;
        Ok(Receipt {
            id: response,
            thread_id: None,
        })
    }
}
