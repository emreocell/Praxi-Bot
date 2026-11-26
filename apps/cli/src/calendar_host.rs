//! Calendars of `bot serve`: `<data dir>/calendar.toml`, the `calendar_*` tools
//! and the `/v1/calendar/*` settings API (`daemon_api::CalendarControl`).
//!
//! Passwords live in the secret store only; the file holds the reference
//! (`password = "calendar/home/password"`), never the value. The settings
//! screen and a hand-edited file are the same thing: the file is read at start
//! and rewritten (comments are not kept) when an account is added, changed or removed.
//!
//! ```toml
//! [[calendar]]
//! name = "home"
//! url = "https://dav.example.com/calendars/me/personal/"
//! username = "me"
//! password = "calendar/home/password"
//! email = "me@example.com"          # optional: the address invitations come from
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use connector_calendar::{
    CalendarAccount, CalendarClient, CalendarConnector, CalendarError, InviteMailer,
};
use connector_email::guard::MailFacts;
use connector_email::{CalendarPart, EmailConnector, OutgoingMail};
use daemon_api::calendar::{
    CalendarAccountDto, CalendarAccountInput, CalendarControl, CalendarControlError,
    CalendarFoundDto, CalendarTestInput, CalendarTestReport,
};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde::{Deserialize, Serialize};
use tool_registry::ToolRegistry;

/// Filename of the calendar configuration in `<data_dir>`.
pub const CALENDAR_FILE: &str = "calendar.toml";

const FILE_HEADER: &str = "# Calendars (CalDAV) - edited via Settings > Calendar or by hand.\n# Passwords live in the secret store; only secret names belong here.\n\n";
/// Longest account name.
const MAX_NAME: usize = 40;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CalendarConfigFile {
    #[serde(default, alias = "calendars")]
    calendar: Vec<CalendarAccount>,
}

/// Why the file could not be read.
enum Loaded {
    Ok(Vec<CalendarAccount>),
    Broken(String),
}

fn load(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<CalendarConfigFile>(&text) {
            Ok(cfg) => Loaded::Ok(cfg.calendar),
            Err(e) => Loaded::Broken(format!("{}: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::Ok(Vec::new()),
        Err(e) => Loaded::Broken(format!("cannot read {}: {e}", path.display())),
    }
}

fn warn(message: &str) {
    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {message}");
}

/// Sends invitations through the bot's e-mail account.
struct EmailInvites {
    email: Arc<EmailConnector>,
}

#[async_trait]
impl InviteMailer for EmailInvites {
    async fn send(
        &self,
        bot: Option<&str>,
        to: &[String],
        subject: &str,
        body: &str,
        ics: &str,
        method: &str,
    ) -> Result<(), String> {
        let mail = self.email.scoped(bot);
        let (account, from) = mail
            .resolve_sender(None)
            .map_err(|_| "no e-mail account is set up for this bot".to_owned())?;
        // The owner's rules for outgoing mail apply to invitations too: an address that asked
        // not to be written to, or a daily limit, stops the mail (there is no card to override).
        let report = self.email.guard().check(&MailFacts {
            bot,
            account: &account,
            from: &from,
            recipients: to,
            opt_out_line: false,
        });
        if let Some(w) = report.warnings.first() {
            return Err(w.text.clone());
        }
        let outgoing = OutgoingMail {
            to: to.to_vec(),
            subject: subject.to_owned(),
            body: body.to_owned(),
            calendar: Some(CalendarPart {
                method: method.to_owned(),
                ics: ics.to_owned(),
            }),
            ..OutgoingMail::default()
        };
        let receipt = mail
            .send_receipt(Some(&account), &outgoing)
            .await
            .map_err(|e| e.to_string())?;
        for r in to {
            self.email.guard().record_system_mail(
                "calendar-invite",
                &account,
                &from,
                r,
                subject,
                &receipt.id,
            );
        }
        Ok(())
    }
}

/// Manages `calendar.toml`, calendar passwords in the secret store and the
/// live [`CalendarConnector`] the registered tools use.
pub struct CalendarHost {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    connector: Arc<CalendarConnector>,
    /// Serialises changes (read-modify-write of the file and the connector).
    ops: tokio::sync::Mutex<()>,
}

/// Register the calendar tools (they stay hidden from bots while no calendar is
/// usable) and return the settings host. `None` without a secret store.
/// `email` lets the tools send invitations by mail when the calendar server does not.
pub fn start_calendar(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
    email: Option<&Arc<EmailConnector>>,
) -> Option<Arc<CalendarHost>> {
    let store = store?;
    let path = data_dir.join(CALENDAR_FILE);
    let accounts = match load(&path) {
        Loaded::Ok(a) => a,
        Loaded::Broken(why) => {
            warn(&why);
            Vec::new()
        }
    };
    let connector = Arc::new(CalendarConnector::empty());
    for p in connector.replace_accounts(accounts, store) {
        warn(&p);
    }
    if let Some(email) = email {
        connector.set_mailer(Arc::new(EmailInvites {
            email: Arc::clone(email),
        }));
    }
    let added = connector_calendar::register(registry, &connector);
    warn(&format!("calendar: {added} tool(s) registered"));
    Some(Arc::new(CalendarHost {
        path,
        secrets: Arc::clone(store),
        connector,
        ops: tokio::sync::Mutex::new(()),
    }))
}

fn invalid(m: impl Into<String>) -> CalendarControlError {
    CalendarControlError::Invalid(m.into())
}

fn internal(m: impl Into<String>) -> CalendarControlError {
    CalendarControlError::Internal(m.into())
}

fn secret_name(account: &str) -> String {
    format!("calendar/{account}/password")
}

fn valid_name(name: &str) -> Result<(), CalendarControlError> {
    if name.is_empty()
        || name.len() > MAX_NAME
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid(
            "the name must be 1 to 40 letters, digits, `-` or `_`",
        ));
    }
    Ok(())
}

/// The plain-words version of a failed server call.
fn friendly(e: &CalendarError) -> String {
    let text = e.to_string();
    if text.contains("refused the login") {
        "The server refused the login. Check the user name and the app password.".to_owned()
    } else if text.contains("not found") {
        "The server does not know this address. Check the address and try again.".to_owned()
    } else if text.contains("error sending request") || text.contains("timed out") {
        "Could not reach the server. Check the address and your internet connection.".to_owned()
    } else {
        text
    }
}

fn dto(a: &CalendarAccount, store: &Arc<dyn SecretStore>) -> CalendarAccountDto {
    let name: String = a.password.clone().into();
    CalendarAccountDto {
        name: a.name.clone(),
        url: a.url.clone(),
        username: a.username.clone(),
        email: a.email.clone(),
        password_set: store.get(&name).ok().flatten().is_some(),
    }
}

impl CalendarHost {
    /// The live connector the tools use.
    #[cfg(test)]
    #[must_use]
    pub fn connector(&self) -> &Arc<CalendarConnector> {
        &self.connector
    }

    fn accounts(&self) -> Result<Vec<CalendarAccount>, CalendarControlError> {
        match load(&self.path) {
            Loaded::Ok(a) => Ok(a),
            // Never overwrite a file the owner edited and we cannot read.
            Loaded::Broken(why) => Err(internal(format!(
                "{why}; fix or remove the file, then try again"
            ))),
        }
    }

    fn persist(&self, accounts: &[CalendarAccount]) -> Result<(), CalendarControlError> {
        let body = toml::to_string_pretty(&CalendarConfigFile {
            calendar: accounts.to_vec(),
        })
        .map_err(|e| internal(format!("cannot serialize {CALENDAR_FILE}: {e}")))?;
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
            internal(format!("cannot save {}: {e}", self.path.display()))
        })
    }

    fn reload(&self, accounts: Vec<CalendarAccount>) {
        for p in self.connector.replace_accounts(accounts, &self.secrets) {
            warn(&p);
        }
    }

    async fn set_secret(&self, name: String, value: String) -> Result<(), CalendarControlError> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || store.set(&name, &value))
            .await
            .map_err(|e| internal(format!("secret task failed: {e}")))?
            .map_err(|e| internal(format!("cannot store the password: {e}")))
    }

    async fn drop_secret(&self, name: String) {
        let store = Arc::clone(&self.secrets);
        let _ = tokio::task::spawn_blocking(move || store.delete(&name)).await;
    }

    /// The password of a saved account.
    async fn stored_password(&self, account: &CalendarAccount) -> Option<String> {
        let (store, name) = (
            Arc::clone(&self.secrets),
            String::from(account.password.clone()),
        );
        tokio::task::spawn_blocking(move || store.get(&name).ok().flatten())
            .await
            .ok()
            .flatten()
            .map(|v| v.expose().to_string())
    }

    /// Check a candidate account against the server and return the address of
    /// its calendar. A server address that holds one calendar resolves to it.
    async fn resolve(
        &self,
        mut account: CalendarAccount,
        password: &str,
    ) -> Result<CalendarAccount, CalendarControlError> {
        let client = trial_client(&account, password)?;
        let found = client.discover().await.map_err(|e| invalid(friendly(&e)))?;
        match found.as_slice() {
            [] => Err(invalid(
                "No calendars were found at this address. Check it, or use Test connection.",
            )),
            [one] => {
                account.url.clone_from(&one.url);
                Ok(account)
            }
            many => {
                let own = account.url.trim_end_matches('/');
                if many.iter().any(|c| c.url.trim_end_matches('/') == own) {
                    return Ok(account);
                }
                let names: Vec<&str> = many.iter().map(|c| c.name.as_str()).collect();
                Err(invalid(format!(
                    "This address holds {} calendars ({}). Use Test connection and pick one.",
                    many.len(),
                    names.join(", ")
                )))
            }
        }
    }

    fn account_from(
        name: &str,
        input: &CalendarAccountInput,
    ) -> Result<CalendarAccount, CalendarControlError> {
        valid_name(name)?;
        let email = input
            .email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| {
                connector_calendar::ical::clean_address(e)
                    .map_err(|_| invalid("the e-mail address is not valid"))
            })
            .transpose()?;
        Ok(CalendarAccount {
            name: name.to_owned(),
            url: input.url.trim().to_owned(),
            username: input.username.trim().to_owned(),
            password: SecretRef::new(secret_name(name))
                .map_err(|e| invalid(format!("bad name: {e}")))?,
            email,
            timeout_secs: 20,
        })
    }
}

/// A client for an account whose password is only in memory (a test, or a
/// candidate that is not saved yet).
fn trial_client(
    account: &CalendarAccount,
    password: &str,
) -> Result<CalendarClient, CalendarControlError> {
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    store
        .set(account.password.name(), password)
        .map_err(|e| internal(format!("cannot hold the password: {e}")))?;
    CalendarClient::new(account.clone(), store).map_err(|e| invalid(e.to_string()))
}

#[async_trait]
impl CalendarControl for CalendarHost {
    fn list(&self) -> Vec<CalendarAccountDto> {
        match load(&self.path) {
            Loaded::Ok(a) => a.iter().map(|a| dto(a, &self.secrets)).collect(),
            Loaded::Broken(why) => {
                warn(&why);
                Vec::new()
            }
        }
    }

    async fn create(
        &self,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError> {
        let _guard = self.ops.lock().await;
        let mut accounts = self.accounts()?;
        let name = input.name.trim().to_owned();
        let account = Self::account_from(&name, &input)?;
        if accounts.iter().any(|a| a.name == name) {
            return Err(CalendarControlError::Conflict(format!(
                "a calendar named `{name}` already exists"
            )));
        }
        let password = input.password.clone().unwrap_or_default();
        let account = self.resolve(account, &password).await?;
        self.set_secret(secret_name(&name), password).await?;
        accounts.push(account.clone());
        if let Err(e) = self.persist(&accounts) {
            self.drop_secret(secret_name(&name)).await;
            return Err(e);
        }
        self.reload(accounts);
        Ok(dto(&account, &self.secrets))
    }

    async fn update(
        &self,
        name: &str,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError> {
        let _guard = self.ops.lock().await;
        let mut accounts = self.accounts()?;
        let at = accounts
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| CalendarControlError::NotFound(format!("calendar {name}")))?;
        let mut account = Self::account_from(name, &input)?;
        // Keep a hand-written secret reference and timeout.
        account.password = accounts[at].password.clone();
        account.timeout_secs = accounts[at].timeout_secs;
        let new_password = input.password.clone();
        let password = match &new_password {
            Some(p) => p.clone(),
            None => self.stored_password(&accounts[at]).await.ok_or_else(|| {
                invalid("no password is stored for this calendar; enter it again")
            })?,
        };
        let account = self.resolve(account, &password).await?;
        if let Some(p) = new_password {
            self.set_secret(String::from(account.password.clone()), p)
                .await?;
        }
        accounts[at] = account.clone();
        self.persist(&accounts)?;
        self.reload(accounts);
        Ok(dto(&account, &self.secrets))
    }

    async fn delete(&self, name: &str) -> Result<(), CalendarControlError> {
        let _guard = self.ops.lock().await;
        let mut accounts = self.accounts()?;
        let at = accounts
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| CalendarControlError::NotFound(format!("calendar {name}")))?;
        let gone = accounts.remove(at);
        self.persist(&accounts)?;
        self.reload(accounts);
        self.drop_secret(String::from(gone.password)).await;
        Ok(())
    }

    async fn test(
        &self,
        input: CalendarTestInput,
    ) -> Result<CalendarTestReport, CalendarControlError> {
        let fail = |error: String| CalendarTestReport {
            ok: false,
            calendars: Vec::new(),
            scheduling: false,
            error: Some(error),
        };
        let client = match (&input.name, &input.url) {
            // A saved account is always tried at its own address with its own password:
            // a stored password never goes to an address the caller supplies.
            (Some(name), None) => {
                let account = self
                    .accounts()?
                    .into_iter()
                    .find(|a| &a.name == name)
                    .ok_or_else(|| CalendarControlError::NotFound(format!("calendar {name}")))?;
                CalendarClient::new(account, Arc::clone(&self.secrets))
                    .map_err(|e| invalid(e.to_string()))?
            }
            (None, Some(url)) => {
                let (Some(user), Some(password)) = (&input.username, &input.password) else {
                    return Err(invalid("a user name and password are needed to test"));
                };
                let account = CalendarAccount {
                    name: "test".into(),
                    url: url.trim().to_owned(),
                    username: user.trim().to_owned(),
                    password: SecretRef::new("calendar/test/password")
                        .map_err(|e| internal(e.to_string()))?,
                    email: None,
                    timeout_secs: 20,
                };
                match trial_client(&account, password) {
                    Ok(c) => c,
                    Err(CalendarControlError::Invalid(m)) => return Ok(fail(m)),
                    Err(e) => return Err(e),
                }
            }
            (Some(_), Some(_)) => {
                return Err(invalid(
                    "give either a saved calendar's name or an address, not both",
                ));
            }
            (None, None) => return Err(invalid("nothing to test")),
        };
        match client.discover().await {
            Ok(found) => Ok(CalendarTestReport {
                ok: true,
                scheduling: client.supports_scheduling().await,
                calendars: found
                    .into_iter()
                    .map(|c| CalendarFoundDto {
                        name: c.name,
                        url: c.url,
                    })
                    .collect(),
                error: None,
            }),
            Err(e) => Ok(fail(friendly(&e))),
        }
    }
}

#[cfg(test)]
#[path = "calendar_host_tests.rs"]
mod tests;
