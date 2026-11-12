//! [`EmailConnector`]: accounts + secret store + the high-level operations.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use secret_store::SecretStore;

use crate::agentmail::{AgentMailClient, AgentMailEndpoint, AgentMailProvider, ApiKey};
use crate::config::{AccountConfig, AccountKind};
use crate::cred::resolve_secret;
use crate::error::EmailError;
use crate::imap::{FolderInfo, SearchQuery};
use crate::imap_provider::ImapSmtpProvider;
use crate::message::{MessageDetail, MessageSummary};
use crate::probe::ProbeReport;
use crate::provider::{
    AttachmentData, Capabilities, MailboxProvider, NewMessages, Receipt, SearchRequest,
};
use crate::smtp::OutgoingMail;

/// Largest attachment downloaded and saved.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

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
    endpoint: AgentMailEndpoint,
    /// Built on first use (TLS roots are loaded then, not at start-up).
    client: RwLock<Option<AgentMailClient>>,
    /// Where downloaded attachments are saved (`None` = downloads refused).
    attachments_dir: RwLock<Option<PathBuf>>,
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
        Self::reloadable_with(secrets, AgentMailEndpoint::production())
    }

    /// Like [`EmailConnector::reloadable`] with an explicit `AgentMail`
    /// endpoint (production, or a loopback mock in tests).
    #[must_use]
    pub fn reloadable_with(secrets: Arc<dyn SecretStore>, endpoint: AgentMailEndpoint) -> Self {
        Self {
            accounts: RwLock::new(Arc::new(BTreeMap::new())),
            secrets,
            endpoint,
            client: RwLock::new(None),
            attachments_dir: RwLock::new(None),
        }
    }

    /// Set the directory attachments are saved in (created on first use).
    pub fn set_attachments_dir(&self, dir: PathBuf) {
        *self
            .attachments_dir
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(dir);
    }

    fn agentmail_client(&self) -> Result<AgentMailClient, EmailError> {
        if let Some(c) = self
            .client
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            return Ok(c.clone());
        }
        let c = AgentMailClient::new(self.endpoint.clone())?;
        *self.client.write().unwrap_or_else(PoisonError::into_inner) = Some(c.clone());
        Ok(c)
    }

    /// The provider serving `acct`.
    ///
    /// # Errors
    /// Credential or config failures (an `AgentMail` account resolves its key
    /// here).
    pub async fn provider_for(
        &self,
        acct: &AccountConfig,
    ) -> Result<Box<dyn MailboxProvider>, EmailError> {
        match acct.kind {
            AccountKind::ImapSmtp => Ok(Box::new(ImapSmtpProvider::new(
                acct.clone(),
                Arc::clone(&self.secrets),
            ))),
            AccountKind::AgentMail => {
                let key = resolve_secret(&self.secrets, &acct.password).await?;
                Ok(Box::new(self.agentmail_for(&key, Some(acct))?))
            }
        }
    }

    /// An `AgentMail` provider for `key`, bound to the inbox of `acct` when
    /// given (without one it can only create an inbox).
    ///
    /// # Errors
    /// [`EmailError::Input`] for a malformed key.
    pub fn agentmail_for(
        &self,
        key: &str,
        acct: Option<&AccountConfig>,
    ) -> Result<AgentMailProvider, EmailError> {
        let inbox = acct.and_then(|a| a.inbox_id.clone().map(|id| (id, a.address.clone())));
        AgentMailProvider::new(self.agentmail_client()?, ApiKey::new(key)?, inbox)
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
        resolve_secret(&self.secrets, &acct.password).await
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

    /// "Test connection" for a stored API-backed account: one cheap
    /// authenticated call, nothing read or sent.
    ///
    /// # Errors
    /// [`EmailError::Input`] for an unknown account, credential and provider
    /// failures otherwise.
    pub async fn check_saved(&self, name: &str) -> Result<(), EmailError> {
        let acct = self
            .snapshot()
            .get(name)
            .cloned()
            .ok_or_else(|| EmailError::Input(format!("unknown account {name:?}")))?;
        self.provider_for(&acct).await?.check().await
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

    async fn provider(
        &self,
        account: Option<&str>,
    ) -> Result<Box<dyn MailboxProvider>, EmailError> {
        let acct = self.account(account)?;
        self.conn.provider_for(&acct).await
    }

    /// What the mailbox of this call can do.
    ///
    /// # Errors
    /// [`EmailError::Input`] when no account applies.
    pub fn capabilities(&self, account: Option<&str>) -> Result<Capabilities, EmailError> {
        let acct = self.account(account)?;
        Ok(match acct.kind {
            AccountKind::ImapSmtp => {
                ImapSmtpProvider::new(acct, Arc::clone(&self.conn.secrets)).capabilities()
            }
            AccountKind::AgentMail => self
                .conn
                .agentmail_for("placeholder-key", Some(&acct))?
                .capabilities(),
        })
    }

    /// The kind of the account a call with `account` would use.
    ///
    /// # Errors
    /// [`EmailError::Input`] as for any call.
    pub fn kind(&self, account: Option<&str>) -> Result<AccountKind, EmailError> {
        self.account(account).map(|a| a.kind)
    }

    /// List folders.
    ///
    /// # Errors
    /// See [`EmailError`]; [`EmailError::Unsupported`] for mailboxes without folders.
    pub async fn list_folders(&self, account: Option<&str>) -> Result<Vec<FolderInfo>, EmailError> {
        self.provider(account).await?.list_folders().await
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
        let found = self
            .provider(account)
            .await?
            .search(&SearchRequest {
                folder,
                query,
                limit,
            })
            .await?;
        Ok(SearchResult {
            messages: found.messages,
            total: found.total,
        })
    }

    /// Fetch a message by IMAP UID (does not mark it read).
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
        if uid == 0 {
            return Err(EmailError::Input("uid must be nonzero".into()));
        }
        self.fetch_id(account, folder, &uid.to_string(), max_body_chars)
            .await
    }

    /// Fetch a message by the provider id (does not mark it read).
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn fetch_id(
        &self,
        account: Option<&str>,
        folder: &str,
        id: &str,
        max_body_chars: usize,
    ) -> Result<MessageDetail, EmailError> {
        self.provider(account)
            .await?
            .get_message(folder, id, max_body_chars)
            .await
    }

    /// Mark a message read (or unread) by IMAP UID.
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
        if uid == 0 {
            return Err(EmailError::Input("uid must be nonzero".into()));
        }
        self.mark_read_id(account, folder, &uid.to_string(), read)
            .await
    }

    /// Mark a message read (or unread) by the provider id.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn mark_read_id(
        &self,
        account: Option<&str>,
        folder: &str,
        id: &str,
        read: bool,
    ) -> Result<(), EmailError> {
        self.provider(account)
            .await?
            .mark_read(folder, id, read)
            .await
    }

    /// Save a draft to the account drafts folder. Nothing is sent.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn save_draft(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        self.provider(account).await?.save_draft(mail).await
    }

    /// Send a message **immediately**. Callers must have obtained approval.
    /// Returns the server reply (SMTP) or the provider message id.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn send(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<String, EmailError> {
        Ok(self.send_receipt(account, mail).await?.id)
    }

    /// Like [`Scoped::send`] with the full receipt.
    ///
    /// # Errors
    /// See [`EmailError`].
    pub async fn send_receipt(
        &self,
        account: Option<&str>,
        mail: &OutgoingMail,
    ) -> Result<Receipt, EmailError> {
        self.provider(account).await?.send(mail).await
    }

    /// Reply in the thread of a message. **Sends immediately**; callers must
    /// have obtained approval.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] for mailboxes without thread replies.
    pub async fn reply(
        &self,
        account: Option<&str>,
        message_id: &str,
        mail: &OutgoingMail,
        reply_all: bool,
    ) -> Result<Receipt, EmailError> {
        self.provider(account)
            .await?
            .reply(message_id, mail, reply_all)
            .await
    }

    /// Messages that arrived after `cursor` (providers with cursor polling).
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] for IMAP accounts.
    pub async fn list_new(
        &self,
        account: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<NewMessages, EmailError> {
        self.provider(account).await?.list_new(cursor, limit).await
    }

    /// Download an attachment (at most [`MAX_ATTACHMENT_BYTES`]) and save it
    /// under the attachments directory with a generated name (the declared
    /// file name is never used for the path, and nothing is ever executed).
    /// Returns the saved path and the data.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] without provider support,
    /// [`EmailError::Config`] when no attachments directory is configured.
    pub async fn save_attachment(
        &self,
        account: Option<&str>,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<(PathBuf, AttachmentData), EmailError> {
        let acct = self.account(account)?;
        let dir = self
            .conn
            .attachments_dir
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| EmailError::Config("no attachments directory is configured".into()))?
            .join(&acct.name);
        let data = self
            .conn
            .provider_for(&acct)
            .await?
            .get_attachment(message_id, attachment_id, MAX_ATTACHMENT_BYTES)
            .await?;
        let path = dir.join(format!("{}.bin", uuid::Uuid::now_v7()));
        let bytes = data.bytes.clone();
        let target = path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(target.parent().unwrap_or(&target))?;
            std::fs::write(&target, bytes)
        })
        .await
        .map_err(|_| EmailError::Config("cannot save the attachment".into()))?
        .map_err(|_| EmailError::Config("cannot save the attachment".into()))?;
        Ok((path, data))
    }
}
