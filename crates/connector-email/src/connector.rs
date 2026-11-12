//! [`EmailConnector`]: accounts + secret store + the high-level operations.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, PoisonError, RwLock};

use secret_store::{SecretStore, resolve};

use crate::config::AccountConfig;
use crate::error::EmailError;
use crate::imap::{FolderInfo, ImapSession, SearchQuery};
use crate::message::{MessageDetail, MessageSummary};
use crate::probe::ProbeReport;
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
/// are never kept, logged or returned. The account set can be replaced at
/// runtime ([`EmailConnector::replace_accounts`]); a call already in flight
/// keeps the snapshot it started with, the next call sees the new set.
pub struct EmailConnector {
    accounts: RwLock<Arc<BTreeMap<String, AccountConfig>>>,
    secrets: Arc<dyn SecretStore>,
}

/// A bound account is usable only by its bot (ids compare case-insensitively,
/// they are UUIDs); an unbound one by every caller. `bot == None` (not a bot)
/// is never "every bot": it reaches unbound accounts only.
fn usable_by(acct: &AccountConfig, bot: Option<&str>) -> bool {
    match &acct.bot {
        None => true,
        Some(owner) => bot.is_some_and(|b| owner.eq_ignore_ascii_case(b)),
    }
}

fn owned_by(acct: &AccountConfig, bot: Option<&str>) -> bool {
    match (&acct.bot, bot) {
        (Some(owner), Some(b)) => owner.eq_ignore_ascii_case(b),
        _ => false,
    }
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
        let c = Self::reloadable(secrets);
        c.replace_accounts(accounts)?;
        Ok(c)
    }

    /// A connector that starts with no accounts and is filled (and changed) at
    /// runtime with [`EmailConnector::replace_accounts`].
    #[must_use]
    pub fn reloadable(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            accounts: RwLock::new(Arc::new(BTreeMap::new())),
            secrets,
        }
    }

    /// Atomically replace the whole account set (may be empty). Everything is
    /// validated first; on error the previous set stays in force.
    ///
    /// # Errors
    /// [`EmailError::Config`] for an invalid or duplicate account.
    pub fn replace_accounts(&self, accounts: Vec<AccountConfig>) -> Result<(), EmailError> {
        let mut map = BTreeMap::new();
        for a in accounts {
            a.validate()?;
            if map.insert(a.name.clone(), a).is_some() {
                return Err(EmailError::Config("duplicate account name".into()));
            }
        }
        *self
            .accounts
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(map);
        Ok(())
    }

    fn snapshot(&self) -> Arc<BTreeMap<String, AccountConfig>> {
        Arc::clone(&self.accounts.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// The current accounts (name order).
    #[must_use]
    pub fn accounts(&self) -> Vec<AccountConfig> {
        self.snapshot().values().cloned().collect()
    }

    /// Configured account names (all of them; for the host, not for bots).
    #[must_use]
    pub fn account_names(&self) -> Vec<String> {
        self.snapshot().keys().cloned().collect()
    }

    /// Whether at least one account is usable by `bot`.
    #[must_use]
    pub fn has_account_for(&self, bot: Option<&str>) -> bool {
        self.snapshot().values().any(|a| usable_by(a, bot))
    }

    /// Operations on behalf of `bot` (`None` = a caller that is not a bot, which
    /// only reaches unbound accounts).
    #[must_use]
    pub fn scoped<'a>(&'a self, bot: Option<&'a str>) -> Scoped<'a> {
        Scoped { conn: self, bot }
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

    /// One deadline around secret resolution and the network call.
    async fn deadline<T>(
        limit: std::time::Duration,
        secs: u64,
        fut: impl Future<Output = Result<T, EmailError>>,
    ) -> Result<T, EmailError> {
        match tokio::time::timeout(limit, fut).await {
            Ok(result) => result,
            Err(_) => Err(EmailError::Timeout(secs)),
        }
    }

    /// Check the password of `acct` is in the secret store (the value is not
    /// returned).
    ///
    /// # Errors
    /// [`EmailError::Credential`] when it cannot be resolved.
    pub async fn password_present(&self, acct: &AccountConfig) -> Result<(), EmailError> {
        self.password(acct).await.map(|_| ())
    }

    /// "Test connection" for a stored account: IMAP login and SMTP
    /// authentication, nothing sent or read (see [`crate::probe`]).
    ///
    /// # Errors
    /// [`EmailError::Input`] for an unknown account, [`EmailError::Credential`]
    /// when its password cannot be resolved. A failing server is a normal
    /// [`ProbeReport`].
    pub async fn probe_saved(&self, name: &str) -> Result<ProbeReport, EmailError> {
        let acct = self
            .snapshot()
            .get(name)
            .cloned()
            .ok_or_else(|| EmailError::Input(format!("unknown account {name:?}")))?;
        let password = self.password(&acct).await?;
        Ok(crate::probe::probe(&acct, &password).await)
    }

    /// List folders (not bot-scoped; see [`EmailConnector::scoped`]).
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn list_folders(&self, account: Option<&str>) -> Result<Vec<FolderInfo>, EmailError> {
        self.scoped(None).list_folders(account).await
    }

    /// Search a folder (not bot-scoped).
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
        self.scoped(None)
            .search(account, folder, query, limit)
            .await
    }

    /// Fetch a message, does not mark it read (not bot-scoped).
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
        self.scoped(None)
            .fetch(account, folder, uid, max_body_chars)
            .await
    }

    /// Mark a message read or unread (not bot-scoped).
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
        self.scoped(None)
            .mark_read(account, folder, uid, read)
            .await
    }

    /// Save a draft; nothing is sent (not bot-scoped).
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn save_draft(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        self.scoped(None).save_draft(account, mail).await
    }

    /// Send a message **immediately** (not bot-scoped). Callers must have
    /// obtained approval.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn send(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        self.scoped(None).send(account, mail).await
    }
}

/// [`EmailConnector`] operations on behalf of one bot. See
/// [`EmailConnector::scoped`].
pub struct Scoped<'a> {
    conn: &'a EmailConnector,
    bot: Option<&'a str>,
}

impl Scoped<'_> {
    /// Pick the account for a call.
    ///
    /// With a name: it must exist *and* be usable by this bot, otherwise the
    /// error is the same "unknown account" for a missing account and for one
    /// bound to another bot (no oracle for other bots' accounts).
    /// Without a name: the bot's own bound account, else the single unbound
    /// one. Names listed in errors are only those this bot can use.
    fn account(&self, name: Option<&str>) -> Result<AccountConfig, EmailError> {
        let all = self.conn.snapshot();
        if let Some(n) = name {
            return all
                .get(n)
                .filter(|a| usable_by(a, self.bot))
                .cloned()
                .ok_or_else(|| EmailError::Input(format!("unknown account {n:?}")));
        }
        let mine: Vec<&AccountConfig> = all.values().filter(|a| owned_by(a, self.bot)).collect();
        let pool: Vec<&AccountConfig> = if mine.is_empty() {
            all.values().filter(|a| a.bot.is_none()).collect()
        } else {
            mine
        };
        match pool.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(EmailError::Input(
                "no e-mail account is available to this bot".into(),
            )),
            many => Err(EmailError::Input(format!(
                "several accounts configured, pass `account` (one of: {})",
                many.iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Name of the account a call with `account` would use.
    ///
    /// # Errors
    /// [`EmailError::Input`] as for any call.
    pub fn resolve_name(&self, account: Option<&str>) -> Result<String, EmailError> {
        self.account(account).map(|a| a.name)
    }

    async fn with_imap<T, F>(&self, account: Option<&str>, f: F) -> Result<T, EmailError>
    where
        F: for<'a> FnOnce(
            &'a mut ImapSession,
            &'a AccountConfig,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, EmailError>> + Send + 'a>,
        >,
    {
        let acct = self.account(account)?;
        self.with_imap_for(acct, f).await
    }

    async fn with_imap_for<T, F>(&self, acct: AccountConfig, f: F) -> Result<T, EmailError>
    where
        F: for<'a> FnOnce(
            &'a mut ImapSession,
            &'a AccountConfig,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, EmailError>> + Send + 'a>,
        >,
    {
        let limit = acct.timeout();
        let secs = limit.as_secs();
        EmailConnector::deadline(limit, secs, async {
            let pw = self.conn.password(&acct).await?;
            let mut s = ImapSession::connect(&acct, &pw).await?;
            let r = f(&mut s, &acct).await;
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
        crate::imap::check_folder(folder)?;
        let folder = folder.to_owned();
        let query = query.clone();
        let (messages, total) = self
            .with_imap(account, move |s, _| {
                Box::pin(async move { s.search(&folder, &query, limit).await })
            })
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
        crate::imap::check_folder(folder)?;
        if uid == 0 {
            return Err(EmailError::Input("uid must be nonzero".into()));
        }
        let folder = folder.to_owned();
        self.with_imap(account, move |s, _| {
            Box::pin(async move { s.fetch(&folder, uid, max_body_chars).await })
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
        crate::imap::check_folder(folder)?;
        if uid == 0 {
            return Err(EmailError::Input("uid must be nonzero".into()));
        }
        let folder = folder.to_owned();
        self.with_imap(account, move |s, _| {
            Box::pin(async move { s.set_seen(&folder, uid, read).await })
        })
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
        let acct = self.account(account)?;
        let raw = smtp::compose_draft(&acct.address, mail)?.formatted();
        self.with_imap_for(acct, |s, acct| {
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
        let limit = acct.timeout();
        let secs = limit.as_secs();
        EmailConnector::deadline(limit, secs, async {
            let pw = self.conn.password(&acct).await?;
            smtp::send(&acct, &pw, msg).await
        })
        .await
    }
}
