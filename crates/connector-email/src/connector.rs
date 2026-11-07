//! [`EmailConnector`]: accounts + secret store + the high-level operations.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use secret_store::{SecretStore, resolve};

use crate::config::AccountConfig;
use crate::error::EmailError;
use crate::imap::{FolderInfo, ImapSession, SearchQuery};
use crate::message::{MessageDetail, MessageSummary};
use crate::smtp::{self, OutgoingMail};

/// Result of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Newest-first page of matches.
    pub messages: Vec<MessageSummary>,
    /// Total matches in the folder (may exceed `messages.len()`).
    pub total: usize,
}

/// Holds account configs and resolves their credentials lazily.
///
/// Passwords are fetched from the [`SecretStore`] at connection time only; they
/// are never kept, logged or returned.
pub struct EmailConnector {
    accounts: BTreeMap<String, AccountConfig>,
    secrets: Arc<dyn SecretStore>,
}

impl EmailConnector {
    /// Create a connector.
    ///
    /// # Errors
    /// [`EmailError::Config`] for invalid or duplicate accounts, or no accounts.
    pub fn new(
        accounts: Vec<AccountConfig>,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, EmailError> {
        if accounts.is_empty() {
            return Err(EmailError::Config("no e-mail accounts configured".into()));
        }
        let mut map = BTreeMap::new();
        for a in accounts {
            a.validate()?;
            if map.insert(a.name.clone(), a).is_some() {
                return Err(EmailError::Config("duplicate account name".into()));
            }
        }
        Ok(Self {
            accounts: map,
            secrets,
        })
    }

    /// Configured account names.
    #[must_use]
    pub fn account_names(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }

    fn account(&self, name: Option<&str>) -> Result<&AccountConfig, EmailError> {
        match name {
            Some(n) => self
                .accounts
                .get(n)
                .ok_or_else(|| EmailError::Input(format!("unknown account {n:?}"))),
            None if self.accounts.len() == 1 => Ok(self.accounts.values().next().expect("one")),
            None => Err(EmailError::Input(format!(
                "several accounts configured, pass `account` (one of: {})",
                self.account_names().join(", ")
            ))),
        }
    }

    async fn password(&self, acct: &AccountConfig) -> Result<String, EmailError> {
        let store = self.secrets.clone();
        let r = acct.password.clone();
        tokio::task::spawn_blocking(move || {
            resolve(store.as_ref(), &r).map(|v| v.expose().to_string())
        })
        .await
        .map_err(|e| EmailError::Credential(e.to_string()))?
        .map_err(|e| EmailError::Credential(e.to_string()))
    }

    async fn timed<T>(
        acct: &AccountConfig,
        fut: impl Future<Output = Result<T, EmailError>>,
    ) -> Result<T, EmailError> {
        tokio::time::timeout(acct.timeout(), fut)
            .await
            .map_err(|_| EmailError::Timeout(acct.timeout().as_secs()))?
    }

    async fn with_imap<T, F>(&self, account: Option<&str>, f: F) -> Result<T, EmailError>
    where
        F: for<'a> FnOnce(
            &'a mut ImapSession,
            &'a AccountConfig,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<T, EmailError>> + Send + 'a>>,
    {
        let acct = self.account(account)?;
        let pw = self.password(acct).await?;
        Self::timed(acct, async {
            let mut s = ImapSession::connect(acct, &pw).await?;
            let r = f(&mut s, acct).await;
            s.logout().await;
            r
        })
        .await
    }

    /// List folders.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn list_folders(&self, account: Option<&str>) -> Result<Vec<FolderInfo>, EmailError> {
        self.with_imap(account, |s, _| Box::pin(s.list_folders()))
            .await
    }

    /// Search a folder.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn search(
        &self,
        account: Option<&str>,
        folder: &str,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<SearchResult, EmailError> {
        query.to_imap()?; // validate before connecting
        let (messages, total) = self
            .with_imap(account, |s, _| Box::pin(s.search(folder, query, limit)))
            .await?;
        Ok(SearchResult { messages, total })
    }

    /// Fetch a message (does not mark it read).
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn fetch(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        self.with_imap(account, |s, _| {
            Box::pin(s.fetch(folder, uid, max_body_chars))
        })
        .await
    }

    /// Mark a message read (or unread).
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn mark_read(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        read: bool,
    ) -> Result<(), EmailError> {
        self.with_imap(account, |s, _| Box::pin(s.set_seen(folder, uid, read)))
            .await
    }

    /// Save a draft to the account's drafts folder. Nothing is sent.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn save_draft(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        let from = self.account(account)?.address.clone();
        let raw = smtp::compose(&from, mail)?.formatted();
        self.with_imap(account, |s, acct| {
            Box::pin(async move {
                s.append_draft(&acct.drafts_folder, &raw).await?;
                Ok(acct.drafts_folder.clone())
            })
        })
        .await
    }

    /// Send a message **immediately**. Callers must have obtained approval.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn send(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        let acct = self.account(account)?;
        let msg = smtp::compose(&acct.address, mail)?;
        let pw = self.password(acct).await?;
        Self::timed(acct, smtp::send(acct, &pw, msg)).await
    }
}
