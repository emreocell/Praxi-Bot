//! E-mail accounts of `bot serve`: loads `<data dir>/email.toml`, registers the
//! `email_*` tools once over a reloadable connector, and implements the
//! `/v1/email/*` management API ([`daemon_api::EmailControl`]).
//!
//! Every change is validated, saved to `email.toml` (temp file, fsync, rename)
//! and only then applied to the running connector, so it takes effect for the
//! next tool call without a restart. Passwords live in the secret store only
//! (`email/<account>/password`); `email.toml` holds the reference, never the
//! value.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, CreateInbox, EmailConnector, EmailError,
    ImapSettings, ProbeReport, ProbeStatus, ProviderError, SmtpSecurity, SmtpSettings,
    normalize_sender, validate_account_name,
};
use daemon_api::email::{
    EmailAccountDto, EmailAccountInput, EmailAccountPatch, EmailImapServer, EmailProbeStatus,
    EmailProviderKind, EmailSecurity, EmailSmtpServer, EmailTestReport,
};
use daemon_api::{EmailControl, EmailControlError};
use secret_store::{SecretRef, SecretStore};
use serde::Serialize;
use tool_registry::ToolRegistry;

use crate::serve::{EMAIL_FILE, EmailConfigFile};

/// Names of managed passwords whose deletion failed (JSON array), retried on start.
const CLEANUP_FILE: &str = "email-secret-cleanup.json";

const FILE_HEADER: &str = "\
# E-mail accounts of this bot (managed by the app; you may edit it while the daemon is stopped).\n\
# Passwords and API keys are never stored here: `password` is the NAME of a secret in the secret store.\n\
# `bot` binds an account to one bot (its storage id); without it every bot that may use the\n\
# e-mail tools can use the account.\n\n";

/// The running e-mail accounts of a daemon.
pub struct EmailHost {
    connector: Arc<EmailConnector>,
    secrets: Arc<dyn SecretStore>,
    path: PathBuf,
    /// Managed secrets that could not be deleted yet (retried on start).
    cleanup_path: PathBuf,
    /// Serialises changes (read-modify-write of the account list and the file).
    ops: tokio::sync::Mutex<()>,
    /// Set when `email.toml` could not be loaded: it is never overwritten then.
    broken: Option<String>,
}

#[derive(Serialize)]
struct FileOut<'a> {
    accounts: &'a [AccountConfig],
}

/// Name under which the daemon stores the password of IMAP/SMTP account `name`.
#[must_use]
pub fn managed_secret(name: &str) -> String {
    format!("email/{name}/password")
}

/// Name under which the daemon stores the API key of `AgentMail` account `name`.
#[must_use]
pub fn managed_api_key(name: &str) -> String {
    format!("email/{name}/api_key")
}

/// The secret the daemon manages for an account of `kind`.
fn managed_secret_for(name: &str, kind: AccountKind) -> String {
    match kind {
        AccountKind::ImapSmtp => managed_secret(name),
        AccountKind::AgentMail => managed_api_key(name),
    }
}

/// A provider failure as an API error: coarse text only (never the provider's
/// words, never a secret).
fn upstream(e: &EmailError) -> EmailControlError {
    match e {
        EmailError::Input(m) | EmailError::Config(m) => EmailControlError::Invalid(m.clone()),
        EmailError::Credential(_) => invalid("no API key is stored for this account"),
        other => EmailControlError::Upstream(other.to_string()),
    }
}

fn probe_status_of(e: &EmailError) -> EmailProbeStatus {
    match e {
        EmailError::Provider(ProviderError::Auth | ProviderError::Forbidden) => {
            EmailProbeStatus::AuthFailed
        }
        EmailError::Provider(ProviderError::Transient | ProviderError::RateLimited { .. }) => {
            EmailProbeStatus::Unreachable
        }
        EmailError::Timeout(_) => EmailProbeStatus::Timeout,
        _ => EmailProbeStatus::Protocol,
    }
}

fn api_report(r: &Result<(), EmailError>) -> EmailTestReport {
    let status = match r {
        Ok(()) => EmailProbeStatus::Ok,
        Err(e) => probe_status_of(e),
    };
    EmailTestReport {
        imap: status,
        smtp: status,
        ok: status == EmailProbeStatus::Ok,
        api: Some(status),
    }
}

/// Directory attachments are saved in, under the data dir.
pub const ATTACHMENTS_DIR: &str = "email-attachments";

fn invalid(e: impl std::fmt::Display) -> EmailControlError {
    EmailControlError::Invalid(e.to_string())
}

fn internal(what: &str) -> EmailControlError {
    EmailControlError::Internal(what.to_owned())
}

/// Load the accounts: `email.toml` (a parse error is an error, so the file is
/// never overwritten by accident), else the legacy `[email]` of `providers.toml`.
fn load(data_dir: &Path) -> Result<Vec<AccountConfig>, String> {
    let path = data_dir.join(EMAIL_FILE);
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let cfg: EmailConfigFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(cfg.accounts);
    }
    Ok(crate::serve::load_email_accounts(data_dir))
}

/// Build the connector over `email.toml`, register the tools (always, once; a
/// bot without a usable account simply does not see them) and return the host
/// for `/v1/email/*`. `None` without a secret store.
pub fn start_email(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> Option<(Arc<EmailHost>, usize)> {
    start_email_with(data_dir, registry, store, AgentMailEndpoint::production())
}

/// [`start_email`] with an explicit `AgentMail` endpoint (a loopback mock in
/// tests).
pub fn start_email_with(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
    endpoint: AgentMailEndpoint,
) -> Option<(Arc<EmailHost>, usize)> {
    let Some(store) = store else {
        eprintln!("warning: e-mail needs a secret store, which is not available");
        return None;
    };
    let host = EmailHost::open_with(data_dir, Arc::clone(store), endpoint);
    let added = connector_email::register(registry, &host.connector);
    let count = host.connector.account_names().len();
    eprintln!("email: {added} tool(s) registered ({count} account(s))");
    Some((host, added))
}

/// Test helper with the old shape: how many tools were registered.
#[cfg(test)]
pub fn register_email(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> usize {
    start_email(data_dir, registry, store).map_or(0, |(_, added)| added)
}

fn security_in(s: EmailSecurity) -> SmtpSecurity {
    match s {
        EmailSecurity::Implicit => SmtpSecurity::Implicit,
        EmailSecurity::StartTls => SmtpSecurity::StartTls,
    }
}

fn security_out(s: SmtpSecurity) -> EmailSecurity {
    match s {
        SmtpSecurity::Implicit => EmailSecurity::Implicit,
        SmtpSecurity::StartTls => EmailSecurity::StartTls,
    }
}

fn status_out(s: ProbeStatus) -> EmailProbeStatus {
    match s {
        ProbeStatus::Ok => EmailProbeStatus::Ok,
        ProbeStatus::Unreachable => EmailProbeStatus::Unreachable,
        ProbeStatus::Tls => EmailProbeStatus::Tls,
        ProbeStatus::AuthFailed => EmailProbeStatus::AuthFailed,
        ProbeStatus::Protocol => EmailProbeStatus::Protocol,
        ProbeStatus::Timeout => EmailProbeStatus::Timeout,
    }
}

fn report_out(r: ProbeReport) -> EmailTestReport {
    EmailTestReport {
        imap: status_out(r.imap),
        smtp: status_out(r.smtp),
        ok: r.all_ok(),
        api: None,
    }
}

/// Everything a password is sent to: if any of it changes, the stored password
/// must not be reused silently (that would let an API caller point the stored
/// password at a server of their choosing).
fn credential_target(a: &AccountConfig) -> (String, u16, String, u16, String) {
    (
        a.imap.host.to_ascii_lowercase(),
        a.imap.port,
        a.smtp.host.to_ascii_lowercase(),
        a.smtp.port,
        // An `AgentMail` key only ever goes to the pinned API host; what can
        // differ is the inbox it is used on.
        a.inbox_id.clone().unwrap_or_else(|| a.login().to_owned()),
    )
}

fn senders(list: &[String]) -> Result<Vec<String>, EmailControlError> {
    let mut out: Vec<String> = Vec::new();
    for s in list {
        let n = normalize_sender(s).map_err(invalid)?;
        if !out.contains(&n) {
            out.push(n);
        }
    }
    Ok(out)
}

impl EmailHost {
    /// Load `email.toml` into a fresh reloadable connector.
    #[cfg(test)]
    fn open(data_dir: &Path, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        Self::open_with(data_dir, secrets, AgentMailEndpoint::production())
    }

    fn open_with(
        data_dir: &Path,
        secrets: Arc<dyn SecretStore>,
        endpoint: AgentMailEndpoint,
    ) -> Arc<Self> {
        let connector = Arc::new(EmailConnector::reloadable_with(
            Arc::clone(&secrets),
            endpoint,
        ));
        connector.set_attachments_dir(data_dir.join(ATTACHMENTS_DIR));
        let mut broken = None;
        match load(data_dir) {
            Ok(accounts) => {
                if let Err(e) = connector.replace_accounts(accounts) {
                    eprintln!("warning: cannot initialize email connector: {e}");
                    broken = Some(e.to_string());
                }
            }
            Err(e) => {
                eprintln!("warning: {e}");
                broken = Some(e);
            }
        }
        let host = Arc::new(Self {
            connector,
            secrets,
            path: data_dir.join(EMAIL_FILE),
            cleanup_path: data_dir.join(CLEANUP_FILE),
            ops: tokio::sync::Mutex::new(()),
            broken,
        });
        host.retry_pending_deletes();
        host
    }

    fn pending_deletes(&self) -> Vec<String> {
        std::fs::read_to_string(&self.cleanup_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_pending(&self, names: &[String]) {
        if names.is_empty() {
            let _ = std::fs::remove_file(&self.cleanup_path);
            return;
        }
        let tmp = self.cleanup_path.with_extension("json.tmp");
        let ok = serde_json::to_vec(names).is_ok_and(|b| std::fs::write(&tmp, b).is_ok())
            && std::fs::rename(&tmp, &self.cleanup_path).is_ok();
        if !ok {
            eprintln!("email: warning: could not record secrets still to delete");
        }
    }

    /// Delete managed passwords whose earlier deletion failed, unless an account
    /// uses the name again. Names that still fail stay on the list.
    fn retry_pending_deletes(&self) {
        let pending = self.pending_deletes();
        if pending.is_empty() {
            return;
        }
        let in_use: Vec<String> = self
            .connector
            .accounts()
            .iter()
            .map(|a| a.password.name().to_owned())
            .collect();
        let mut left = Vec::new();
        for name in pending {
            if in_use.contains(&name) {
                continue;
            }
            if self.secrets.delete(&name).is_err() {
                left.push(name);
            }
        }
        self.save_pending(&left);
    }

    fn dto(&self, a: &AccountConfig) -> EmailAccountDto {
        EmailAccountDto {
            name: a.name.clone(),
            provider: match a.kind {
                AccountKind::ImapSmtp => EmailProviderKind::ImapSmtp,
                AccountKind::AgentMail => EmailProviderKind::Agentmail,
            },
            inbox_id: a.inbox_id.clone(),
            address: a.address.clone(),
            username: a.username.clone(),
            imap: EmailImapServer {
                host: a.imap.host.clone(),
                port: a.imap.port,
            },
            smtp: EmailSmtpServer {
                host: a.smtp.host.clone(),
                port: a.smtp.port,
                security: security_out(a.smtp.security),
            },
            drafts_folder: a.drafts_folder.clone(),
            bot: a.bot.clone(),
            allowed_senders: a.allowed_senders.clone(),
            password_set: self.secrets.get(a.password.name()).ok().flatten().is_some(),
            custom_ca: a.extra_root_pem.is_some(),
        }
    }

    /// Save `accounts` to `email.toml` (temp file, fsync, rename).
    fn persist(&self, accounts: &[AccountConfig]) -> Result<(), EmailControlError> {
        if let Some(why) = &self.broken {
            return Err(EmailControlError::Internal(format!(
                "email.toml could not be loaded ({why}); fix or remove it before changing accounts"
            )));
        }
        let body = toml::to_string_pretty(&FileOut { accounts })
            .map_err(|_| internal("cannot serialize the e-mail accounts"))?;
        let tmp = self.path.with_extension("toml.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(FILE_HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            EmailControlError::Internal(format!("cannot save {}: {e}", self.path.display()))
        })
    }

    /// Save, then apply to the running connector.
    fn commit(&self, accounts: Vec<AccountConfig>) -> Result<(), EmailControlError> {
        self.persist(&accounts)?;
        self.connector.replace_accounts(accounts).map_err(invalid)
    }

    async fn store_secret(&self, name: String, value: String) -> Result<(), EmailControlError> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || store.set(&name, &value))
            .await
            .map_err(|_| internal("cannot store the password"))?
            .map_err(|_| internal("cannot store the password"))
    }

    async fn read_secret(&self, name: String) -> Option<String> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || {
            store
                .get(&name)
                .ok()
                .flatten()
                .map(|v| v.expose().to_owned())
        })
        .await
        .ok()
        .flatten()
    }

    /// Delete the passwords the daemon created for `removed`, unless another
    /// account still uses the same secret. A deletion that fails is recorded and
    /// retried on the next start.
    async fn drop_managed_secrets(&self, removed: &[AccountConfig]) {
        let still_used: Vec<String> = self
            .connector
            .accounts()
            .iter()
            .map(|a| a.password.name().to_owned())
            .collect();
        let mut pending = self.pending_deletes();
        let before = pending.len();
        for a in removed {
            let secret = a.password.name().to_owned();
            if secret != managed_secret_for(&a.name, a.kind) || still_used.contains(&secret) {
                continue;
            }
            let store = Arc::clone(&self.secrets);
            let name = secret.clone();
            let r = tokio::task::spawn_blocking(move || store.delete(&name)).await;
            if !matches!(r, Ok(Ok(_))) {
                eprintln!(
                    "email: warning: could not delete the password of account {}; it will be retried on the next start",
                    a.name
                );
                if !pending.contains(&secret) {
                    pending.push(secret);
                }
            }
        }
        if pending.len() != before {
            self.save_pending(&pending);
        }
    }

    /// A name that is about to be used again must not be deleted by a retry.
    fn unpend(&self, secret: &str) {
        let mut pending = self.pending_deletes();
        let n = pending.len();
        pending.retain(|p| p != secret);
        if pending.len() != n {
            self.save_pending(&pending);
        }
    }

    /// The account a draft test connects with: the request, plus the private CA
    /// of the saved account of the same name when it targets the same servers
    /// and login (a custom CA cannot be set through the API).
    fn draft_account(&self, input: &EmailAccountInput) -> Result<AccountConfig, EmailControlError> {
        if input.provider == EmailProviderKind::Agentmail {
            return Err(invalid("a draft test of an AgentMail account is not saved"));
        }
        let mut account = Self::account_from(input)?;
        if let Some(stored) = self
            .connector
            .accounts()
            .into_iter()
            .find(|a| a.name == account.name)
            .filter(|s| credential_target(s) == credential_target(&account))
        {
            account.extra_root_pem = stored.extra_root_pem;
        }
        Ok(account)
    }

    /// Build an account from a request body.
    fn account_from(input: &EmailAccountInput) -> Result<AccountConfig, EmailControlError> {
        validate_account_name(&input.name).map_err(invalid)?;
        let password = SecretRef::new(managed_secret(&input.name))
            .map_err(|_| invalid("invalid account name for a secret"))?;
        let account = AccountConfig {
            name: input.name.clone(),
            address: input.address.trim().to_owned(),
            username: input.username.clone().unwrap_or_default().trim().to_owned(),
            password,
            imap: ImapSettings {
                host: input.imap.host.trim().to_owned(),
                port: input.imap.port,
            },
            smtp: SmtpSettings {
                host: input.smtp.host.trim().to_owned(),
                port: input.smtp.port,
                security: security_in(input.smtp.security),
            },
            drafts_folder: input
                .drafts_folder
                .clone()
                .unwrap_or_else(|| "Drafts".to_owned()),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: input.bot.clone(),
            allowed_senders: senders(&input.allowed_senders)?,
            kind: AccountKind::ImapSmtp,
            inbox_id: None,
        };
        account.validate().map_err(invalid)?;
        Ok(account)
    }
}

impl EmailHost {
    /// Create the inbox at `AgentMail` with the user's key, then store the key
    /// and save the account. A failure after the inbox exists takes the new
    /// inbox back (it is one this very call created).
    async fn add_agentmail(
        &self,
        input: EmailAccountInput,
    ) -> Result<EmailAccountDto, EmailControlError> {
        validate_account_name(&input.name).map_err(invalid)?;
        let key = input
            .api_key
            .clone()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| invalid("an API key is required"))?;
        let secret = SecretRef::new(managed_api_key(&input.name))
            .map_err(|_| invalid("invalid account name for a secret"))?;
        let _guard = self.ops.lock().await;
        let mut list = self.connector.accounts();
        if list.iter().any(|a| a.name == input.name) {
            return Err(EmailControlError::Conflict(format!(
                "account {} already exists",
                input.name
            )));
        }
        let creator = self
            .connector
            .agentmail_for(&key, None)
            .map_err(|e| upstream(&e))?;
        let inbox = connector_email::MailboxProvider::create_inbox(
            &creator,
            &CreateInbox {
                name_hint: input.name_hint.clone().or_else(|| Some(input.name.clone())),
                domain: input.domain.clone(),
                client_id: format!("praxi-{}", uuid::Uuid::now_v7()),
            },
        )
        .await
        .map_err(|e| upstream(&e))?;
        let account = AccountConfig {
            name: input.name.clone(),
            address: inbox.address.clone(),
            username: String::new(),
            password: secret,
            imap: ImapSettings::default(),
            smtp: SmtpSettings::default(),
            drafts_folder: "Drafts".to_owned(),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: input.bot.clone(),
            allowed_senders: senders(&input.allowed_senders)?,
            kind: AccountKind::AgentMail,
            inbox_id: Some(inbox.inbox_id.clone()),
        };
        let stored = match account.validate().map_err(invalid) {
            Ok(()) => {
                let name = account.password.name().to_owned();
                self.store_secret(name.clone(), key.clone())
                    .await
                    .map(|()| self.unpend(&name))
            }
            Err(e) => Err(e),
        };
        let result = match stored {
            Ok(()) => {
                list.push(account.clone());
                self.commit(list)
            }
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            self.drop_managed_secrets(std::slice::from_ref(&account))
                .await;
            // Best effort: the user never saw this inbox, so do not leave it
            // counting against their plan.
            if let Ok(p) = self.connector.agentmail_for(&key, Some(&account)) {
                let _ = connector_email::MailboxProvider::delete_inbox(&p).await;
            }
            return Err(e);
        }
        Ok(self.dto(&account))
    }

    /// Test an `AgentMail` key: the one in the request, else the stored key of
    /// the saved account with the same name (which must be an `AgentMail` one).
    async fn test_agentmail_key(
        &self,
        input: &EmailAccountInput,
    ) -> Result<EmailTestReport, EmailControlError> {
        let key = if let Some(k) = input.api_key.clone().filter(|k| !k.is_empty()) {
            k
        } else {
            let stored = self
                .connector
                .accounts()
                .into_iter()
                .find(|a| a.name == input.name && a.kind == AccountKind::AgentMail)
                .ok_or_else(|| invalid("enter the API key to test this account"))?;
            self.read_secret(stored.password.name().to_owned())
                .await
                .ok_or_else(|| invalid("no API key is stored for this account"))?
        };
        let provider = self
            .connector
            .agentmail_for(&key, None)
            .map_err(|e| upstream(&e))?;
        let checked = connector_email::MailboxProvider::check(&provider).await;
        Ok(api_report(&checked))
    }
}

#[async_trait]
impl EmailControl for EmailHost {
    fn list(&self) -> Vec<EmailAccountDto> {
        self.connector
            .accounts()
            .iter()
            .map(|a| self.dto(a))
            .collect()
    }

    async fn add(&self, input: EmailAccountInput) -> Result<EmailAccountDto, EmailControlError> {
        if input.provider == EmailProviderKind::Agentmail {
            return self.add_agentmail(input).await;
        }
        let password = input
            .password
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| invalid("a password is required"))?;
        let account = Self::account_from(&input)?;
        let _guard = self.ops.lock().await;
        let mut list = self.connector.accounts();
        if list.iter().any(|a| a.name == account.name) {
            return Err(EmailControlError::Conflict(format!(
                "account {} already exists",
                account.name
            )));
        }
        // Secret first, so a failure leaves nothing half-configured; it is
        // taken back again if the file cannot be written.
        let secret = account.password.name().to_owned();
        self.store_secret(secret.clone(), password).await?;
        self.unpend(&secret);
        list.push(account.clone());
        if let Err(e) = self.commit(list) {
            self.drop_managed_secrets(std::slice::from_ref(&account))
                .await;
            return Err(e);
        }
        Ok(self.dto(&account))
    }

    async fn update(
        &self,
        name: &str,
        patch: serde_json::Value,
    ) -> Result<EmailAccountDto, EmailControlError> {
        let raw = patch
            .as_object()
            .ok_or_else(|| invalid("the body must be a JSON object"))?;
        let bot_present = raw.contains_key("bot");
        let typed: EmailAccountPatch = serde_json::from_value(patch.clone()).map_err(invalid)?;
        let _guard = self.ops.lock().await;
        let mut list = self.connector.accounts();
        let slot = list
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or_else(|| EmailControlError::NotFound(format!("email account {name}")))?;
        let before = credential_target(slot);
        let api = slot.kind == AccountKind::AgentMail;
        if api
            && (typed.address.is_some()
                || typed.username.is_some()
                || typed.imap.is_some()
                || typed.smtp.is_some()
                || typed.drafts_folder.is_some()
                || typed.password.is_some())
        {
            return Err(invalid(
                "an AgentMail account only takes `bot`, `allowed_senders` and `api_key`",
            ));
        }
        if !api && typed.api_key.is_some() {
            return Err(invalid("`api_key` is only for AgentMail accounts"));
        }
        if let Some(v) = typed.address {
            v.trim().clone_into(&mut slot.address);
        }
        if let Some(v) = typed.username {
            v.trim().clone_into(&mut slot.username);
        }
        if let Some(v) = typed.imap {
            slot.imap = ImapSettings {
                host: v.host.trim().to_owned(),
                port: v.port,
            };
        }
        if let Some(v) = typed.smtp {
            slot.smtp = SmtpSettings {
                host: v.host.trim().to_owned(),
                port: v.port,
                security: security_in(v.security),
            };
        }
        if let Some(v) = typed.drafts_folder {
            slot.drafts_folder = v;
        }
        if bot_present {
            slot.bot = typed.bot;
        }
        if let Some(v) = typed.allowed_senders {
            slot.allowed_senders = senders(&v)?;
        }
        slot.validate().map_err(invalid)?;
        let password = if api { typed.api_key } else { typed.password }.filter(|p| !p.is_empty());
        if password.is_none() && credential_target(slot) != before {
            return Err(invalid(
                "changing the servers or the login requires entering the password again",
            ));
        }
        let secret = slot.password.name().to_owned();
        let updated = slot.clone();
        // New password first: if storing it fails nothing has changed, and if
        // the commit fails the old password is put back, so the live servers
        // and the stored password never disagree for longer than this call.
        let old = if let Some(p) = &password {
            let old = self.read_secret(secret.clone()).await;
            self.store_secret(secret.clone(), p.clone()).await?;
            Some(old)
        } else {
            None
        };
        if let Err(e) = self.commit(list) {
            match old {
                Some(Some(previous)) => {
                    let _ = self.store_secret(secret, previous).await;
                }
                Some(None) => {
                    let store = Arc::clone(&self.secrets);
                    let _ = tokio::task::spawn_blocking(move || store.delete(&secret)).await;
                }
                None => {}
            }
            return Err(e);
        }
        Ok(self.dto(&updated))
    }

    async fn remove(&self, name: &str) -> Result<(), EmailControlError> {
        self.remove_account(name, false).await
    }

    async fn remove_account(
        &self,
        name: &str,
        delete_remote: bool,
    ) -> Result<(), EmailControlError> {
        let _guard = self.ops.lock().await;
        let list = self.connector.accounts();
        let Some(gone) = list.iter().find(|a| a.name == name).cloned() else {
            return Err(EmailControlError::NotFound(format!("email account {name}")));
        };
        if delete_remote {
            if gone.kind != AccountKind::AgentMail {
                return Err(invalid("this account has no inbox at a provider to delete"));
            }
            // Remote first: if the provider refuses, the account stays so the
            // user can try again. An inbox that is already gone counts as done.
            let key = self
                .read_secret(gone.password.name().to_owned())
                .await
                .ok_or_else(|| invalid("no API key is stored for this account"))?;
            let provider = self
                .connector
                .agentmail_for(&key, Some(&gone))
                .map_err(|e| upstream(&e))?;
            match connector_email::MailboxProvider::delete_inbox(&provider).await {
                Ok(()) | Err(EmailError::NotFound(_)) => {}
                Err(e) => return Err(upstream(&e)),
            }
        }
        let rest: Vec<_> = list.into_iter().filter(|a| a.name != name).collect();
        self.commit(rest)?;
        self.drop_managed_secrets(&[gone]).await;
        Ok(())
    }

    async fn test_saved(&self, name: &str) -> Result<EmailTestReport, EmailControlError> {
        let kind = self
            .connector
            .accounts()
            .into_iter()
            .find(|a| a.name == name)
            .map(|a| a.kind);
        if kind == Some(AccountKind::AgentMail) {
            return match self.connector.check_saved(name).await {
                Err(EmailError::Credential(_)) => {
                    Err(invalid("no API key is stored for this account"))
                }
                r => Ok(api_report(&r)),
            };
        }
        match self.connector.probe_saved(name).await {
            Ok(r) => Ok(report_out(r)),
            Err(EmailError::Input(_)) => {
                Err(EmailControlError::NotFound(format!("email account {name}")))
            }
            Err(EmailError::Credential(_)) => {
                Err(invalid("no password is stored for this account"))
            }
            Err(_) => Err(internal("the connection test failed")),
        }
    }

    async fn test_draft(
        &self,
        input: EmailAccountInput,
    ) -> Result<EmailTestReport, EmailControlError> {
        if input.provider == EmailProviderKind::Agentmail {
            return self.test_agentmail_key(&input).await;
        }
        let account = self.draft_account(&input)?;
        let password = if let Some(p) = input.password.clone().filter(|p| !p.is_empty()) {
            p
        } else {
            // Reuse the stored password of the account with this name, but only
            // for the servers and login it is already configured for.
            let stored = self
                .connector
                .accounts()
                .into_iter()
                .find(|a| a.name == account.name)
                .ok_or_else(|| invalid("enter the password to test this account"))?;
            if credential_target(&stored) != credential_target(&account) {
                return Err(invalid(
                    "enter the password to test changed servers or login",
                ));
            }
            self.read_secret(stored.password.name().to_owned())
                .await
                .ok_or_else(|| invalid("no password is stored for this account"))?
        };
        Ok(report_out(
            connector_email::probe(&account, &password).await,
        ))
    }

    async fn forget_bot(&self, bot_id: &str) -> Result<usize, EmailControlError> {
        let _guard = self.ops.lock().await;
        let list = self.connector.accounts();
        let (gone, rest): (Vec<_>, Vec<_>) = list.into_iter().partition(|a| {
            a.bot
                .as_deref()
                .is_some_and(|b| b.eq_ignore_ascii_case(bot_id))
        });
        if gone.is_empty() {
            return Ok(0);
        }
        self.commit(rest)?;
        self.drop_managed_secrets(&gone).await;
        Ok(gone.len())
    }
}

#[cfg(test)]
#[path = "email_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "email_host_agentmail_tests.rs"]
mod agentmail_tests;
