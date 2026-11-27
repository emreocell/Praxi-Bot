//! [`EmailConnector`]: accounts + secret store + the high-level operations.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use secret_store::SecretStore;

use crate::agentmail::{AgentMailClient, AgentMailEndpoint, AgentMailProvider, ApiKey};
use crate::attachment::{self, AttachmentPlace, Sniffed};
use crate::config::{AccountConfig, AccountKind, SendMode};
use crate::cred::resolve_secret;
use crate::error::EmailError;
use crate::guard::MailGuard;
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
/// Name of the folder under the data folder that holds downloaded attachments
/// (one sub-folder per account). What is in it came from a stranger.
pub const ATTACHMENTS_DIR_NAME: &str = "email-attachments";

/// Result of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Newest-first page of matches.
    pub messages: Vec<MessageSummary>,
    /// Total matches in the folder (may exceed `messages.len()`).
    pub total: usize,
    /// API mailboxes: the `page_token` that returns the next page, when there is one.
    pub next_page_token: Option<String>,
    /// The folders that were searched, when more than the asked one was (a search
    /// that included junk); empty otherwise.
    pub folders_searched: Vec<String>,
}

/// An attachment that was saved.
#[derive(Debug, Clone)]
pub struct SavedAttachment {
    /// Where it was written.
    pub path: PathBuf,
    /// The bytes and the declared type and name.
    pub data: AttachmentData,
    /// What the bytes show (the type that counts).
    pub sniffed: Sniffed,
    /// The saved file's name.
    pub file_name: String,
    /// The declared type names something else than the bytes are.
    pub type_conflict: bool,
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
    /// Per-bot place for attachments (the bot's workspace), when the host has one.
    attachment_place: RwLock<Option<Arc<dyn AttachmentPlace>>>,
    /// Outgoing-mail guardrails (send log, opt-out list, limits).
    guard: RwLock<Arc<MailGuard>>,
}

/// `m` marked with the folder it was found in and whether that is a junk folder.
fn tagged(mut m: MessageSummary, folder: &str, junk: bool) -> MessageSummary {
    m.folder = Some(folder.to_owned());
    m.from_junk = Some(junk);
    m
}

fn is_bot(id: &str, bot: Option<&str>) -> bool {
    bot.is_some_and(|b| id.eq_ignore_ascii_case(b))
}

/// A bound account is usable only by its bot and the bots it is shared with
/// (ids compare case-insensitively, they are UUIDs); an unbound one by every
/// caller. `bot == None` (not a bot) is never "every bot": it reaches unbound
/// accounts only.
fn usable_by(acct: &AccountConfig, bot: Option<&str>) -> bool {
    match &acct.bot {
        None => true,
        Some(owner) => {
            is_bot(owner, bot) || acct.identity.shared_with.iter().any(|s| is_bot(s, bot))
        }
    }
}

fn owned_by(acct: &AccountConfig, bot: Option<&str>) -> bool {
    acct.bot.as_deref().is_some_and(|o| is_bot(o, bot))
        || acct.identity.shared_with.iter().any(|s| is_bot(s, bot))
}

/// The account `bot` has marked as its default sender, if it may use it.
fn default_of<'a>(
    all: &'a BTreeMap<String, AccountConfig>,
    bot: Option<&str>,
) -> Option<&'a AccountConfig> {
    all.values()
        .find(|a| usable_by(a, bot) && a.identity.default_for.iter().any(|d| is_bot(d, bot)))
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
            attachment_place: RwLock::new(None),
            guard: RwLock::new(Arc::new(MailGuard::in_memory())),
        }
    }

    /// The outgoing-mail guard (shared; the host reads and edits the log and the lists here).
    #[must_use]
    pub fn guard(&self) -> Arc<MailGuard> {
        Arc::clone(&self.guard.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Keep the send log, opt-out list and settings in `dir` (loads what is there).
    pub fn set_guard_dir(&self, dir: PathBuf) {
        *self.guard.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(MailGuard::open(dir));
    }

    /// Set the directory attachments are saved in (created on first use).
    pub fn set_attachments_dir(&self, dir: PathBuf) {
        *self
            .attachments_dir
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(dir);
    }

    /// Save a bot's attachments where `place` says (its workspace) instead of the
    /// daemon's folder. A bot `place` has no folder for keeps the daemon's folder.
    pub fn set_attachment_place(&self, place: Arc<dyn AttachmentPlace>) {
        *self
            .attachment_place
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(place);
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

    /// An `AgentMail` provider for `key`, bound to an inbox that already
    /// exists (`inbox_id` is its id as the provider names it, normally the
    /// address). Its address is learned from [`AgentMailProvider::get_inbox`].
    ///
    /// # Errors
    /// [`EmailError::Input`] for a malformed key, [`EmailError::Config`] for an
    /// invalid inbox id.
    pub fn agentmail_for_inbox(
        &self,
        key: &str,
        inbox_id: &str,
    ) -> Result<AgentMailProvider, EmailError> {
        AgentMailProvider::new(
            self.agentmail_client()?,
            ApiKey::new(key)?,
            Some((inbox_id.to_owned(), String::new())),
        )
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
        if let Some(default) = default_of(&all, self.bot) {
            return Ok(default.clone());
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

    /// The accounts this bot may use as a sender, with the one a call without
    /// `account` would pick marked. Names and addresses only: no secrets.
    #[must_use]
    pub fn identities(&self) -> Vec<(AccountConfig, bool)> {
        let all = self.conn.snapshot();
        let chosen = self.account(None).ok().map(|a| a.name);
        all.values()
            .filter(|a| usable_by(a, self.bot))
            .map(|a| (a.clone(), chosen.as_deref() == Some(a.name.as_str())))
            .collect()
    }

    /// The identities that are THIS bot's own: accounts bound to it or shared
    /// with it (not the unbound ones every bot may use), with the one a call
    /// without `account` would pick marked. Names and addresses only.
    #[must_use]
    pub fn own_identities(&self) -> Vec<(AccountConfig, bool)> {
        let all = self.conn.snapshot();
        let chosen = self.account(None).ok().map(|a| a.name);
        all.values()
            .filter(|a| owned_by(a, self.bot))
            .map(|a| (a.clone(), chosen.as_deref() == Some(a.name.as_str())))
            .collect()
    }

    /// Whether the account a call with `account` would use is this bot's own
    /// identity (bound to it or shared with it). `false` for an unbound
    /// account, another bot's account and a call that resolves to nothing.
    #[must_use]
    pub fn owns(&self, account: Option<&str>) -> bool {
        self.account(account).is_ok_and(|a| owned_by(&a, self.bot))
    }

    /// The account a call with `account` would use.
    ///
    /// # Errors
    /// [`EmailError::Input`] as for any call.
    pub fn resolve(&self, account: Option<&str>) -> Result<AccountConfig, EmailError> {
        self.account(account)
    }

    /// Name of the account a call with `account` would use.
    ///
    /// # Errors
    /// [`EmailError::Input`] as for any call.
    pub fn resolve_name(&self, account: Option<&str>) -> Result<String, EmailError> {
        self.account(account).map(|a| a.name)
    }

    /// Name and sending address of the account a call would use.
    ///
    /// # Errors
    /// [`EmailError::Input`] as for any call.
    pub fn resolve_sender(&self, account: Option<&str>) -> Result<(String, String), EmailError> {
        self.account(account).map(|a| (a.name, a.address))
    }

    async fn provider(
        &self,
        account: Option<&str>,
    ) -> Result<Box<dyn MailboxProvider>, EmailError> {
        let acct = self.account(account)?;
        self.conn.provider_for(&acct).await
    }

    /// The account of a call that is about to **send**. An identity the owner
    /// limited to drafts never sends, whoever the caller is: the check lives
    /// here so no path (tool, channel, workflow) can skip it.
    fn sendable(&self, account: Option<&str>) -> Result<AccountConfig, EmailError> {
        let acct = self.account(account)?;
        if acct.identity.send_mode == SendMode::DraftOnly {
            return Err(EmailError::Input("draft only".into()));
        }
        Ok(acct)
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
            next_page_token: found.next_page_token,
            folders_searched: Vec::new(),
        })
    }

    /// [`Scoped::search`] that also looks in the junk folders when the query asks for
    /// junk (`include_junk`): mailboxes with folders search the server's junk folder
    /// (flagged `\Junk` or named like one) next to `folder` and tag every message with
    /// the folder it is in and `from_junk`; API mailboxes pass the request on and tag by
    /// the provider's spam label. A junk folder that cannot be searched is skipped; the
    /// answer lists the folders that were.
    ///
    /// # Errors
    /// See [`EmailError`]; only the asked folder's failure fails the call.
    pub async fn search_with_junk(
        &self,
        account: Option<&str>,
        folder: &str,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<SearchResult, EmailError> {
        let provider = self.provider(account).await?;
        let req = SearchRequest {
            folder,
            query,
            limit,
        };
        let first = provider.search(&req).await?;
        if query.include_junk != Some(true) || !provider.capabilities().folders {
            return Ok(SearchResult {
                messages: first.messages,
                total: first.total,
                next_page_token: first.next_page_token,
                folders_searched: Vec::new(),
            });
        }
        let junk = match provider.list_folders().await {
            Ok(list) => crate::wait::junk_folders(&list),
            Err(_) => Vec::new(),
        };
        let asked_is_junk = junk.iter().any(|j| j == folder);
        let mut total = first.total;
        let mut searched = vec![folder.to_owned()];
        let mut messages: Vec<MessageSummary> = first
            .messages
            .into_iter()
            .map(|m| tagged(m, folder, asked_is_junk))
            .collect();
        for j in junk.iter().filter(|j| j.as_str() != folder) {
            let more = provider
                .search(&SearchRequest {
                    folder: j,
                    query,
                    limit,
                })
                .await;
            let Ok(more) = more else { continue };
            searched.push(j.clone());
            total += more.total;
            messages.extend(more.messages.into_iter().map(|m| tagged(m, j, true)));
        }
        // Newest first across folders; mail without a readable date goes last.
        messages.sort_by_key(|m| {
            std::cmp::Reverse(
                m.date
                    .as_deref()
                    .and_then(mail_parser::DateTime::parse_rfc3339)
                    .map(|d| d.to_timestamp()),
            )
        });
        messages.truncate(limit);
        Ok(SearchResult {
            messages,
            total,
            // Folder mailboxes (IMAP) have no page tokens.
            next_page_token: None,
            folders_searched: searched,
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
        let acct = self.sendable(account)?;
        self.conn.provider_for(&acct).await?.send(mail).await
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
        let acct = self.sendable(account)?;
        self.conn
            .provider_for(&acct)
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

    /// Download an attachment (at most [`MAX_ATTACHMENT_BYTES`]) and save it: in
    /// the bot's workspace when the host has an [`AttachmentPlace`] for it
    /// (`downloads/email/<account>/`), else under the attachments directory
    /// (`<account>/`). The file name is the declared one reduced to plain characters
    /// (never a path, never overwriting another file); a file that is or looks like a
    /// program gets `.bin` added; the type is sniffed from the bytes. Nothing is
    /// ever executed.
    ///
    /// # Errors
    /// [`EmailError::Unsupported`] without provider support,
    /// [`EmailError::Config`] when there is no place to save to or the write fails,
    /// [`EmailError::Input`] when the bot's workspace quota forbids the write.
    pub async fn save_attachment(
        &self,
        account: Option<&str>,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<SavedAttachment, EmailError> {
        self.save_attachment_in(account, "INBOX", message_id, attachment_id)
            .await
    }

    /// [`Scoped::save_attachment`] for a message in `folder` (IMAP accounts).
    ///
    /// # Errors
    /// As [`Scoped::save_attachment`].
    pub async fn save_attachment_in(
        &self,
        account: Option<&str>,
        folder: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<SavedAttachment, EmailError> {
        let acct = self.account(account)?;
        // The account name is a path component here; it is checked where it is used too.
        crate::config::validate_account_name(&acct.name)?;
        let place = self
            .conn
            .attachment_place
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let workspace = place.as_ref().and_then(|p| p.folder_for(self.bot));
        let dir = match &workspace {
            Some(ws) => ws.join("downloads").join("email").join(&acct.name),
            None => self
                .conn
                .attachments_dir
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .ok_or_else(|| EmailError::Config("no attachments directory is configured".into()))?
                .join(&acct.name),
        };
        let data = self
            .conn
            .provider_for(&acct)
            .await?
            .get_attachment_in(folder, message_id, attachment_id, MAX_ATTACHMENT_BYTES)
            .await?;
        // The provider enforces the cap while reading; this is the second line.
        if data.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(EmailError::Input(format!(
                "the attachment is larger than {} MiB",
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        let sniffed = attachment::sniff(&data.bytes);
        let file_name = attachment::safe_file_name(data.filename.as_deref(), &sniffed);
        let type_conflict = attachment::type_conflict(data.content_type.as_deref(), &sniffed);
        if let Some(place) = place.as_ref().filter(|_| workspace.is_some()) {
            let len = u64::try_from(data.bytes.len()).unwrap_or(u64::MAX);
            place
                .allow_write(self.bot, &dir.join(&file_name), len)
                .map_err(EmailError::Input)?;
        }
        let bytes = data.bytes.clone();
        let name = file_name.clone();
        let target = dir.clone();
        let path =
            tokio::task::spawn_blocking(move || attachment::write_unique(&target, &name, &bytes))
                .await
                .map_err(|_| EmailError::Config("cannot save the attachment".into()))??;
        if let Some(place) = place.as_ref().filter(|_| workspace.is_some()) {
            place.saved(&path);
        }
        let file_name = path
            .file_name()
            .map_or(file_name, |n| n.to_string_lossy().into_owned());
        Ok(SavedAttachment {
            path,
            data,
            sniffed,
            file_name,
            type_conflict,
        })
    }
}
