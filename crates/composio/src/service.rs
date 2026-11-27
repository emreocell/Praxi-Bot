//! The Composio service: key custody, catalog, connect flow, discovery and
//! execution.  The daemon wraps it in `ComposioControl`; tools call into it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tool_registry::{Tool, ToolRegistry};

use crate::catalog::{self, CATALOG_FILE, CatalogFile, CatalogView};
use crate::client::{
    Account, Auth, CliPoll, ComposioClient, Link, Redact, ToolkitLookup, ToolkitPage,
};
use crate::curated::{Curated, display_class, short_name};
use crate::error::{ComposioError, Result};
use crate::keys::{KeySource, LOGIN_URL, Stored};
use crate::probe::{
    ProbeOutcome, Step, find_string, fit_args, primary_calendar, probe_for, sample_items,
};
use crate::state::{AppConfig, FILE_NAME, LoginIds, StateFile};
use crate::tool::{ActionTool, FindActionTool, RunActionTool};

const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
const CATALOG_TTL: Duration = Duration::from_secs(900);
const CATALOG_CACHE_MAX: usize = 64;
/// How long a consent page may stay unfinished before the app counts as failed
/// (the browser login of Composio expires on the same scale).
const PENDING_TIMEOUT: Duration = Duration::from_secs(600);
/// Pages of 100 toolkits read for the disk cache (the catalog has ~1700).
const CATALOG_PAGES: usize = 30;

/// Clears the "refresh running" flag when the refresh ends, however it ends.
struct RefreshGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for RefreshGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Exclusive use of one app plus a shared hold on the credential, for the length of one
/// operation. The app's entry leaves the lock table again when nobody else waits for it, so
/// the table never outgrows the apps in use.
struct AppGuard<'a> {
    table: &'a Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    slug: String,
    slot: Option<tokio::sync::OwnedMutexGuard<()>>,
    _credential: tokio::sync::RwLockReadGuard<'a, ()>,
}

impl Drop for AppGuard<'_> {
    fn drop(&mut self) {
        let mut table = self.table.lock();
        // The table's own handle and this guard's: no one else is waiting.
        let idle = self
            .slot
            .as_ref()
            .is_some_and(|slot| Arc::strong_count(tokio::sync::OwnedMutexGuard::mutex(slot)) <= 2);
        if idle {
            table.remove(&self.slug);
        }
        drop(self.slot.take());
    }
}

/// Receives secret-free audit rows (`composio.*`).
pub trait Audit: Send + Sync {
    /// Record one row.
    ///
    /// # Errors
    /// The row could not be stored.
    fn record(&self, action: &str, detail: Value) -> std::result::Result<(), String>;
}

/// Connection state of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    /// Nothing connected.
    NotConnected,
    /// A consent page was opened and sign-in is not finished.
    Pending,
    /// Connected; tools exist while the app is enabled.
    Active,
    /// The authorization ran out; reconnect.
    Expired,
    /// The sign-in did not finish (cancelled, refused by the provider or timed
    /// out); try again.
    Failed,
}

/// How Praxi is linked to Composio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linkage {
    /// Not linked.
    None,
    /// Linked through the browser login (the normal, copy-free path).
    Login,
    /// The owner's own Composio project key (advanced).
    ProjectKey,
}

/// State of the one-time browser login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginState {
    /// No login was started.
    Idle,
    /// Waiting for the user in the browser.
    Pending,
    /// Linked.
    Linked,
    /// Expired, refused or failed; start again.
    Failed,
}

/// A started browser login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    /// https URL on Composio's dashboard to open in the system browser.
    pub login_url: String,
    /// ISO timestamp, may be empty.
    pub expires_at: String,
}

struct LoginSession {
    id: String,
    started: Instant,
}

/// A connected (or connecting) app as shown in the Apps panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppView {
    /// Toolkit slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// State.
    pub state: ConnState,
    /// Owner opted in.
    pub enabled: bool,
    /// Praxi connects this natively (or will).
    pub native_first: bool,
    /// Bots allowed; `None` = all.
    pub bots: Option<Vec<String>>,
    /// Direct tools currently registered for the model.
    pub tools: usize,
    /// Owner-facing alias of the bound active account, when Composio supplies one.
    pub account: Option<String>,
}

/// One action discovered from Composio.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionInfo {
    /// Composio tool slug.
    pub tool_slug: String,
    /// Short name inside the toolkit.
    pub name: String,
    /// Concrete version discovered with this schema.
    pub version: String,
    /// Description (untrusted, capped).
    pub description: String,
    /// Sanitized input schema.
    pub schema: Value,
    /// Display class: `read`, `send`, `destructive`, `other`.
    pub class: &'static str,
}

/// Everything the service needs from its embedder.
pub struct ServiceConfig {
    /// Where `composio.toml` lives.
    pub data_dir: PathBuf,
    /// The OS secret store (the key lives here only).
    pub keys: Arc<dyn KeySource>,
    /// Where tools are registered.
    pub registry: ToolRegistry,
    /// The daemon's live redactor.
    pub redact: Redact,
    /// Register a candidate with the production redactor before any request.
    pub register_secret: Arc<dyn Fn(&str) + Send + Sync>,
    /// Audit sink.
    pub audit: Arc<dyn Audit>,
    /// Tests only: a fake server on loopback.
    pub loopback_base: Option<String>,
}

struct Prepared {
    at: Instant,
    token: String,
    account: String,
    action: ActionInfo,
}

/// An action as the owner sees it before connecting: a name and one short line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPreview {
    /// Short name inside the toolkit.
    pub name: String,
    /// One line of Composio's description: redacted, single-spaced, at most
    /// [`PREVIEW_DESCRIPTION_CHARS`] characters, without control or direction-changing characters.
    pub description: String,
}

/// Most actions one preview lists.
pub const PREVIEW_ACTIONS: usize = 100;
/// Longest description line of a preview.
pub const PREVIEW_DESCRIPTION_CHARS: usize = 160;

/// Composio's text as one plain line of at most `max` characters. Control characters, line breaks
/// and the invisible characters that reorder or hide text (bidi marks and overrides, zero-width
/// and joiner characters, the byte order mark) are removed or turned into spaces.
fn preview_line(text: &str, max: usize) -> String {
    let hidden = |c: char| matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
    let plain: String = text
        .chars()
        .filter(|c| !hidden(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", line[..end].trim_end()),
        None => line,
    }
}

type Discovered = (Instant, Arc<Vec<ActionInfo>>);

pub(crate) struct Inner {
    keys: Arc<dyn KeySource>,
    registry: ToolRegistry,
    redact: Redact,
    audit: Arc<dyn Audit>,
    register_secret: Arc<dyn Fn(&str) + Send + Sync>,
    loopback_base: Option<String>,
    path: PathBuf,
    pub(crate) curated: Curated,
    state: Mutex<StateFile>,
    client: Mutex<Option<Arc<ComposioClient>>>,
    client_load: tokio::sync::Mutex<()>,
    key_present: std::sync::atomic::AtomicBool,
    statuses: Mutex<HashMap<String, ConnState>>,
    account_labels: Mutex<HashMap<String, String>>,
    /// Taken for writing by the few operations that swap the credential (key, login,
    /// sign-out) and for reading by every per-app operation, so none runs against a
    /// half-swapped credential. Network calls happen outside the write side.
    credentials: tokio::sync::RwLock<()>,
    /// One lock per app: operations on the same app run one at a time, an app never waits
    /// for another one (see [`AppGuard`]).
    app_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    discovered: Mutex<HashMap<String, Discovered>>,
    registered: Mutex<HashMap<String, Vec<String>>>,
    catalog: Mutex<HashMap<String, (Instant, ToolkitPage)>>,
    catalog_file: Mutex<Option<CatalogFile>>,
    catalog_refreshing: std::sync::atomic::AtomicBool,
    catalog_error: Mutex<Option<&'static str>>,
    catalog_attempt: Mutex<Option<Instant>>,
    /// Composio refused the stored credential (401/403) and nothing renewed it yet.
    rejected: std::sync::atomic::AtomicBool,
    login: Mutex<Option<LoginSession>>,
    router_session: Mutex<Option<String>>,
    pub(crate) pending_since: Mutex<HashMap<String, Instant>>,
    auth_configs: Mutex<HashMap<String, String>>,
    prepared: Mutex<HashMap<String, Prepared>>,
}

/// Cheap to clone; shared by the routes and the tools.
#[derive(Clone)]
pub struct ComposioService(pub(crate) Arc<Inner>);

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl ComposioService {
    /// Load the state file, note whether a key exists and register the two
    /// native search/run tools.  Connected apps are refreshed by
    /// [`Self::refresh_all`] (call it in the background).
    ///
    /// # Errors
    /// `composio.toml` exists but is broken.
    pub fn new(cfg: ServiceConfig) -> Result<Self> {
        let path = cfg.data_dir.join(FILE_NAME);
        let state = StateFile::load(&path)?;
        let key_present = cfg.keys.present();
        let svc = Self(Arc::new(Inner {
            keys: cfg.keys,
            registry: cfg.registry,
            redact: cfg.redact,
            audit: cfg.audit,
            register_secret: cfg.register_secret,
            loopback_base: cfg.loopback_base,
            path,
            curated: Curated::builtin(),
            state: Mutex::new(state),
            client: Mutex::new(None),
            client_load: tokio::sync::Mutex::new(()),
            key_present: key_present.into(),
            statuses: Mutex::new(HashMap::new()),
            account_labels: Mutex::new(HashMap::new()),
            credentials: tokio::sync::RwLock::new(()),
            app_locks: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            registered: Mutex::new(HashMap::new()),
            catalog: Mutex::new(HashMap::new()),
            catalog_file: Mutex::new(CatalogFile::load(&cfg.data_dir.join(CATALOG_FILE))),
            catalog_refreshing: false.into(),
            catalog_error: Mutex::new(None),
            catalog_attempt: Mutex::new(None),
            rejected: false.into(),
            login: Mutex::new(None),
            router_session: Mutex::new(None),
            pending_since: Mutex::new(HashMap::new()),
            auth_configs: Mutex::new(HashMap::new()),
            prepared: Mutex::new(HashMap::new()),
        }));
        for t in [
            Arc::new(FindActionTool::new(svc.clone())) as Arc<dyn Tool>,
            Arc::new(RunActionTool::new(svc.clone())) as Arc<dyn Tool>,
        ] {
            svc.0.registry.register(t);
        }
        Ok(svc)
    }

    /// The shipped table (popular row, native-first list).
    #[must_use]
    pub fn curated(&self) -> &Curated {
        &self.0.curated
    }

    /// Whether a key is stored.
    #[must_use]
    pub fn configured(&self) -> bool {
        self.0
            .key_present
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Composio refused the stored credential and nothing has renewed it
    /// since: the owner has to sign in to Composio again (a retry cannot help).
    #[must_use]
    pub fn login_expired(&self) -> bool {
        self.configured() && self.0.rejected.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Remember whether Composio accepts the credential: a 401/403 marks it
    /// rejected, any success clears the mark.  The result passes through.
    fn note<T>(&self, r: Result<T>) -> Result<T> {
        use std::sync::atomic::Ordering::Relaxed;
        match &r {
            Ok(_) => self.0.rejected.store(false, Relaxed),
            Err(ComposioError::Unauthorized) if self.configured() => {
                if !self.0.rejected.swap(true, Relaxed) {
                    self.audit("composio.login_rejected", json!({}));
                }
            }
            Err(_) => {}
        }
        r
    }

    fn save_state(&self, change: impl FnOnce(&mut StateFile)) -> Result<()> {
        let mut st = self.0.state.lock();
        let mut next = st.clone();
        change(&mut next);
        next.save(&self.0.path)?;
        *st = next;
        Ok(())
    }

    fn user_id(&self) -> String {
        self.0.state.lock().user_id.clone()
    }

    fn audit(&self, action: &str, detail: Value) {
        if let Err(e) = self.0.audit.record(action, detail) {
            tracing_warn(&(self.0.redact)(&e));
        }
    }

    async fn client(&self) -> Result<Arc<ComposioClient>> {
        let _load = self.0.client_load.lock().await;
        if let Some(c) = self.0.client.lock().clone() {
            return Ok(c);
        }
        let stored = self
            .0
            .keys
            .get()
            .await?
            .ok_or(ComposioError::NotConfigured)?;
        let auth = match Stored::parse(&stored) {
            Stored::Project(key) => {
                (self.0.register_secret)(&key);
                Auth::Project(key)
            }
            Stored::User(key) => {
                (self.0.register_secret)(&key);
                let ids = self.repaired_ids(&key).await?;
                Auth::User {
                    key,
                    org_id: ids.org_id,
                    project_id: ids.project_id,
                }
            }
        };
        let c = Arc::new(self.build_with(&auth)?);
        *self.0.client.lock() = Some(Arc::clone(&c));
        Ok(c)
    }

    /// The org and project ids sent with a login key.  Installations that
    /// linked before the project NANO id was read hold the project UUID, which
    /// Composio refuses on every call: read the ids again once and save them.
    ///
    /// When the ids cannot be read (network down, Composio busy) nothing is
    /// saved and the error is returned, so no client is cached with the wrong
    /// ids and the next call tries the repair again.
    async fn repaired_ids(&self, key: &str) -> Result<LoginIds> {
        let ids = self.0.state.lock().login.clone().unwrap_or_default();
        let stale = [&ids.project_id, &ids.org_id]
            .into_iter()
            .any(|id| id.as_deref().is_some_and(crate::client::looks_like_uuid));
        if !stale {
            return Ok(ids);
        }
        let probe = self.build_with(&Auth::User {
            key: key.to_owned(),
            org_id: None,
            project_id: None,
        })?;
        let fresh = match probe.session_info().await {
            Ok(found) => found.ids,
            Err(e) => {
                tracing_warn(&format!(
                    "could not refresh the Composio login ids, will retry: {}",
                    (self.0.redact)(&e.to_string())
                ));
                return Err(e);
            }
        };
        if let Err(e) = self.save_state(|st| st.login = Some(fresh.clone())) {
            tracing_warn(&format!(
                "could not save the refreshed Composio login ids: {}",
                (self.0.redact)(&e.to_string())
            ));
        }
        Ok(fresh)
    }

    fn build_client(&self, key: &str) -> Result<ComposioClient> {
        self.build_with(&Auth::Project(key.to_owned()))
    }

    fn build_with(&self, auth: &Auth) -> Result<ComposioClient> {
        let redact = Arc::clone(&self.0.redact);
        match &self.0.loopback_base {
            Some(base) => ComposioClient::for_loopback_with(auth, base, redact),
            None => ComposioClient::new_with(auth, redact),
        }
    }

    /// How Praxi is linked to Composio right now.
    #[must_use]
    pub fn linkage(&self) -> Linkage {
        if !self.configured() {
            Linkage::None
        } else if self.0.state.lock().login.is_some() {
            Linkage::Login
        } else {
            Linkage::ProjectKey
        }
    }

    fn catalog_path(&self) -> PathBuf {
        self.0.path.with_file_name(CATALOG_FILE)
    }

    /// Exclusive use of `slug` for one operation. `slug` must already be a valid slug.
    async fn app_guard(&self, slug: &str) -> AppGuard<'_> {
        let lock = Arc::clone(self.0.app_locks.lock().entry(slug.to_owned()).or_default());
        let slot = lock.lock_owned().await;
        let credential = self.0.credentials.read().await;
        AppGuard {
            table: &self.0.app_locks,
            slug: slug.to_owned(),
            slot: Some(slot),
            _credential: credential,
        }
    }

    /// [`Self::app_guard`] for an app that is connected (or being connected), re-checked once the
    /// app is held: it may have been disconnected while this call waited.
    async fn known_app_guard(&self, slug: &str) -> Result<AppGuard<'_>> {
        let not_connected = || ComposioError::NotFound(format!("app `{slug}` is not connected"));
        if !self.0.state.lock().apps.contains_key(slug) || !valid_slug(slug) {
            return Err(not_connected());
        }
        let guard = self.app_guard(slug).await;
        if !self.0.state.lock().apps.contains_key(slug) {
            return Err(not_connected());
        }
        Ok(guard)
    }

    /// Forget the pending browser login `id`, unless a newer one replaced it.
    fn clear_login(&self, id: &str) {
        let mut login = self.0.login.lock();
        if login.as_ref().is_some_and(|l| l.id == id) {
            *login = None;
        }
    }

    // --- one-time browser login -----------------------------------------

    /// Start the browser login: Composio makes a session, the caller opens
    /// the returned URL in the system browser and polls [`Self::login_poll`].
    /// No key is typed or copied anywhere.
    ///
    /// # Errors
    /// Composio is unreachable or answers an error.
    pub async fn login_start(&self) -> Result<LoginStart> {
        let anon = self.build_with(&Auth::Anonymous)?;
        let session = anon.create_cli_session().await?;
        let login_url = format!("{LOGIN_URL}?cliKey={}", session.id);
        *self.0.login.lock() = Some(LoginSession {
            id: session.id,
            started: Instant::now(),
        });
        Ok(LoginStart {
            login_url,
            expires_at: session.expires_at,
        })
    }

    /// One check of the browser login.  On success the user key goes straight
    /// to the secret store; it is never returned.
    ///
    /// # Errors
    /// Only local failures (secret store, state file).
    pub async fn login_poll(&self) -> Result<LoginState> {
        let id = {
            let mut g = self.0.login.lock();
            match g.as_ref() {
                None if self.linkage() == Linkage::Login && !self.login_expired() => {
                    return Ok(LoginState::Linked);
                }
                None => return Ok(LoginState::Idle),
                Some(l) if l.started.elapsed() > PENDING_TIMEOUT => {
                    *g = None;
                    return Ok(LoginState::Failed);
                }
                Some(l) => l.id.clone(),
            }
        };
        let anon = self.build_with(&Auth::Anonymous)?;
        let key = match anon.get_cli_session(&id).await {
            Ok(CliPoll::Pending) => return Ok(LoginState::Pending),
            Ok(CliPoll::Linked(key)) => key,
            // A flaky network is not a failed login: keep waiting.
            Err(ComposioError::Transport(_) | ComposioError::RateLimited { .. }) => {
                return Ok(LoginState::Pending);
            }
            Err(_) => {
                self.clear_login(&id);
                return Ok(LoginState::Failed);
            }
        };
        (self.0.register_secret)(&key);
        let ids = match self
            .build_with(&Auth::User {
                key: key.clone(),
                org_id: None,
                project_id: None,
            })?
            .session_info()
            .await
        {
            Ok(info) => info.ids,
            Err(_) => LoginIds::default(),
        };
        // A fresh key that Composio refuses at once is a failed login, not a
        // linked one: tried with the ids first, then with the key alone.
        let mut ids = ids;
        match self.probe_login(&key, &ids).await {
            Err(ComposioError::Transport(_) | ComposioError::RateLimited { .. }) => {
                return Ok(LoginState::Pending);
            }
            Err(ComposioError::Unauthorized) => {
                ids = LoginIds::default();
                match self.probe_login(&key, &ids).await {
                    Err(ComposioError::Unauthorized) => {
                        self.clear_login(&id);
                        return Ok(LoginState::Failed);
                    }
                    Err(ComposioError::Transport(_) | ComposioError::RateLimited { .. }) => {
                        return Ok(LoginState::Pending);
                    }
                    _ => {}
                }
            }
            // Accepted, or any other answer: it says nothing against the key.
            Ok(()) | Err(_) => {}
        }
        // The network calls above ran unlocked; only the swap of the credential is exclusive.
        // A poll that lost the race to another poll, a newer login or a key change stops here.
        let _swap = self.0.credentials.write().await;
        if self.0.login.lock().as_ref().map(|l| l.id.as_str()) != Some(id.as_str()) {
            return Ok(LoginState::Pending);
        }
        let _load = self.0.client_load.lock().await;
        self.0.keys.set(&Stored::encode_user(&key)).await?;
        self.save_state(|st| st.login = Some(ids.clone()))?;
        self.reset_connections();
        *self.0.client.lock() = None;
        *self.0.router_session.lock() = None;
        *self.0.login.lock() = None;
        self.0
            .rejected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.0
            .key_present
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.audit("composio.login_linked", json!({}));
        self.spawn_catalog_refresh();
        let bg = self.clone();
        tokio::spawn(async move { bg.refresh_all().await });
        Ok(LoginState::Linked)
    }

    /// One cheap authenticated call with a login key and these ids.
    async fn probe_login(&self, key: &str, ids: &LoginIds) -> Result<()> {
        self.build_with(&Auth::User {
            key: key.to_owned(),
            org_id: ids.org_id.clone(),
            project_id: ids.project_id.clone(),
        })?
        .list_toolkits(None, None, None, 1)
        .await
        .map(|_| ())
    }

    /// Validate `key` against Composio, then store it.  The key is never
    /// returned, logged or put in an error.
    ///
    /// # Errors
    /// [`ComposioError::Invalid`] for a malformed or rejected key.
    pub async fn set_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.is_empty()
            || key.len() > 512
            || key.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ComposioError::Invalid(
                "the API key must be 1 to 512 characters without spaces".into(),
            ));
        }
        (self.0.register_secret)(key);
        let client = Arc::new(self.build_client(key)?);
        match client.validate_key().await {
            Ok(()) => {}
            Err(ComposioError::Unauthorized) => {
                return Err(ComposioError::Invalid(
                    "Composio rejected this API key".into(),
                ));
            }
            Err(e) => return Err(e),
        }
        let swap = self.0.credentials.write().await;
        let load = self.0.client_load.lock().await;
        self.0.keys.set(key).await?;
        self.reset_connections();
        *self.0.client.lock() = Some(client);
        *self.0.router_session.lock() = None;
        self.0
            .rejected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.0
            .key_present
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.save_state(|st| st.login = None)?;
        *self.0.login.lock() = None;
        self.spawn_catalog_refresh();
        drop(load);
        drop(swap);
        self.refresh_all().await;
        Ok(())
    }

    /// Delete the key and hide every Composio tool.
    ///
    /// # Errors
    /// The secret store failed.
    pub async fn remove_key(&self) -> Result<()> {
        let _swap = self.0.credentials.write().await;
        let _load = self.0.client_load.lock().await;
        self.0.keys.clear().await?;
        *self.0.client.lock() = None;
        self.0
            .rejected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.0
            .key_present
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.reset_connections();
        *self.0.router_session.lock() = None;
        *self.0.login.lock() = None;
        self.save_state(|st| st.login = None)?;
        Ok(())
    }

    fn reset_connections(&self) {
        self.0.statuses.lock().clear();
        self.0.account_labels.lock().clear();
        self.0.auth_configs.lock().clear();
        self.0.pending_since.lock().clear();
        self.0.discovered.lock().clear();
        self.0.prepared.lock().clear();
        let slugs: Vec<String> = self.0.registered.lock().keys().cloned().collect();
        for s in slugs {
            self.unregister_app(&s);
        }
    }

    /// Display identity of the linked Composio organization or project, when available.
    pub async fn account_label(&self) -> Option<String> {
        let mode = self.linkage();
        if mode == Linkage::None || self.login_expired() {
            return None;
        }
        let client = tokio::time::timeout(Duration::from_secs(3), self.client())
            .await
            .ok()?
            .ok()?;
        let label = client.account_label(mode == Linkage::Login).await;
        self.0
            .client
            .lock()
            .as_ref()
            .filter(|current| Arc::ptr_eq(current, &client) && mode == self.linkage())?;
        label
    }

    /// One page of Composio's toolkit catalog (cached for 15 minutes).
    ///
    /// # Errors
    /// Not configured, or Composio errors.
    pub async fn toolkits(
        &self,
        search: Option<&str>,
        category: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ToolkitPage> {
        let key = format!(
            "{}|{}|{}|{limit}",
            search.unwrap_or_default().to_ascii_lowercase(),
            category.unwrap_or_default().to_ascii_lowercase(),
            cursor.unwrap_or_default()
        );
        if let Some((at, page)) = self.0.catalog.lock().get(&key)
            && at.elapsed() < CATALOG_TTL
        {
            return Ok(page.clone());
        }
        let page = self.note(
            self.client()
                .await?
                .list_toolkits(search, category, cursor, limit)
                .await,
        )?;
        let mut cache = self.0.catalog.lock();
        if cache.len() >= CATALOG_CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), page.clone()));
        Ok(page)
    }

    /// The catalog the Apps modal shows, built from memory only: it never
    /// waits for the network and works offline.  Starts a background refresh
    /// when the cache is missing or stale and a credential exists.
    #[must_use]
    pub fn catalog_view(
        &self,
        search: Option<&str>,
        group: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> CatalogView {
        let view = {
            let cache = self.0.catalog_file.lock();
            catalog::view(
                &self.0.curated,
                cache.as_ref(),
                search,
                group,
                cursor,
                limit,
            )
        };
        self.spawn_catalog_refresh();
        view
    }

    /// Whether a background refresh is running.
    #[must_use]
    pub fn catalog_refreshing(&self) -> bool {
        self.0
            .catalog_refreshing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Short reason the last refresh failed (a fixed phrase, never server text).
    #[must_use]
    pub fn catalog_error(&self) -> Option<&'static str> {
        *self.0.catalog_error.lock()
    }

    /// Refresh in the background when due.  Cheap and idempotent.
    pub fn spawn_catalog_refresh(&self) {
        let due = self
            .0
            .catalog_file
            .lock()
            .as_ref()
            .is_none_or(CatalogFile::is_stale)
            || self.catalog_error().is_some();
        // A failed refresh is retried by the next request, at most every 10 s.
        let cooling_down = self.catalog_error().is_some()
            && self
                .0
                .catalog_attempt
                .lock()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10));
        if !due || cooling_down || !self.configured() || self.catalog_refreshing() {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = self.clone();
        rt.spawn(async move {
            let _ = me.refresh_catalog().await;
        });
    }

    /// Read the whole Composio catalog (about 17 requests), keep it in memory
    /// and on disk.  A failure keeps the previous cache and records a short,
    /// fixed reason for the retry hint.
    ///
    /// # Errors
    /// Not configured, Composio errors, disk errors (cache kept as it was).
    pub async fn refresh_catalog(&self) -> Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if self
            .0
            .catalog_refreshing
            .compare_exchange(false, true, SeqCst, SeqCst)
            .is_err()
        {
            return Ok(());
        }
        let _done = RefreshGuard(&self.0.catalog_refreshing);
        *self.0.catalog_attempt.lock() = Some(Instant::now());
        let result = async {
            let client = self.client().await?;
            let mut items: Vec<crate::client::Toolkit> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut cursor: Option<String> = None;
            let mut seen_cursors = std::collections::HashSet::new();
            for _ in 0..CATALOG_PAGES {
                let page = client
                    .list_toolkits(None, None, cursor.as_deref(), 100)
                    .await?;
                items.extend(
                    page.items
                        .into_iter()
                        .filter(|t| seen.insert(t.slug.clone())),
                );
                cursor = page.next_cursor;
                if !cursor
                    .as_ref()
                    .is_some_and(|next| seen_cursors.insert(next.clone()))
                {
                    break;
                }
            }
            if cursor.is_some() {
                return Err(ComposioError::Upstream(
                    "toolkit catalog is incomplete".into(),
                ));
            }
            if items.is_empty() {
                return Err(ComposioError::Upstream("empty catalog".into()));
            }
            let file = CatalogFile {
                updated_at: catalog::now_secs(),
                items,
            };
            file.save(&self.catalog_path())?;
            *self.0.catalog_file.lock() = Some(file);
            Ok::<(), ComposioError>(())
        }
        .await;
        let result = self.note(result);
        *self.0.catalog_error.lock() = result.as_ref().err().map(|e| match e {
            ComposioError::Unauthorized => "unauthorized",
            ComposioError::Transport(_) => "offline",
            ComposioError::RateLimited { .. } => "rate_limited",
            _ => "failed",
        });
        result
    }

    fn view(&self, slug: &str, cfg: &AppConfig) -> AppView {
        let name = self.0.curated.popular_name(slug).map_or_else(
            || {
                self.0
                    .catalog_file
                    .lock()
                    .as_ref()
                    .and_then(|c| c.items.iter().find(|t| t.slug == slug))
                    .map_or_else(|| slug.to_owned(), |t| t.name.clone())
            },
            str::to_owned,
        );
        AppView {
            slug: slug.to_owned(),
            name,
            state: self
                .0
                .statuses
                .lock()
                .get(slug)
                .copied()
                .unwrap_or(ConnState::NotConnected),
            enabled: cfg.enabled,
            native_first: self.0.curated.is_native_first(slug),
            bots: cfg.bots.clone(),
            tools: self.0.registered.lock().get(slug).map_or(0, Vec::len),
            account: self.0.account_labels.lock().get(slug).cloned(),
        }
    }

    /// Apps the owner connected or started to connect (no network).
    #[must_use]
    pub fn apps(&self) -> Vec<AppView> {
        let st = self.0.state.lock().apps.clone();
        st.iter().map(|(slug, cfg)| self.view(slug, cfg)).collect()
    }

    /// Refresh one app from Composio and return it.
    ///
    /// # Errors
    /// Unknown app, not configured, Composio errors.
    pub async fn app_status(&self, slug: &str) -> Result<AppView> {
        self.refresh(slug).await?;
        let cfg = self
            .0
            .state
            .lock()
            .apps
            .get(slug)
            .cloned()
            .ok_or_else(|| ComposioError::NotFound(format!("app `{slug}` is not connected")))?;
        Ok(self.view(slug, &cfg))
    }

    /// Re-read the status of every known app (startup, background).
    pub async fn refresh_all(&self) {
        if !self.configured() {
            return;
        }
        let slugs: Vec<String> = self.0.state.lock().apps.keys().cloned().collect();
        for s in slugs {
            let _ = self.refresh(&s).await;
        }
    }

    fn derive(accounts: &[Account]) -> ConnState {
        let has = |names: &[&str]| accounts.iter().any(|a| names.contains(&a.status.as_str()));
        if has(&["ACTIVE"]) {
            ConnState::Active
        } else if has(&["INITIALIZING", "INITIATED"]) {
            ConnState::Pending
        } else if accounts.is_empty() {
            ConnState::NotConnected
        } else {
            ConnState::Expired
        }
    }

    async fn refresh(&self, slug: &str) -> Result<()> {
        if !valid_slug(slug) {
            return Err(ComposioError::NotFound(format!(
                "app `{slug}` is not connected"
            )));
        }
        let _guard = self.app_guard(slug).await;
        self.refresh_connection(slug).await
    }

    async fn refresh_connection(&self, slug: &str) -> Result<()> {
        if !self.0.state.lock().apps.contains_key(slug) {
            return Err(ComposioError::NotFound(format!(
                "app `{slug}` is not connected"
            )));
        }
        let accounts = self.note(
            self.client()
                .await?
                .list_accounts(&self.user_id(), Some(slug))
                .await,
        )?;
        if let Err(e) = self.check_accounts(slug, &accounts) {
            self.0.statuses.lock().remove(slug);
            self.0.account_labels.lock().remove(slug);
            self.unregister_app(slug);
            return Err(e);
        }
        let mut new = Self::derive(&accounts);
        {
            let mut pending = self.0.pending_since.lock();
            let since = pending.get(slug).copied();
            match (new, since) {
                (ConnState::Active, _) => {
                    pending.remove(slug);
                }
                (ConnState::Pending | ConnState::NotConnected, Some(t)) => {
                    if t.elapsed() > PENDING_TIMEOUT {
                        pending.remove(slug);
                        new = ConnState::Failed;
                    } else {
                        new = ConnState::Pending;
                    }
                }
                (ConnState::Expired, Some(_)) => {
                    pending.remove(slug);
                    new = ConnState::Failed;
                }
                _ => {}
            }
        }
        let old = self.0.statuses.lock().insert(slug.to_owned(), new);
        {
            let mut labels = self.0.account_labels.lock();
            if new == ConnState::Active
                && let Some(label) = accounts.into_iter().find_map(|a| a.label)
            {
                labels.insert(slug.to_owned(), label);
            } else {
                labels.remove(slug);
            }
        }
        if old != Some(new) {
            match new {
                ConnState::Active => self.audit("composio.connected", json!({"app": slug})),
                ConnState::Expired => self.audit("composio.expired", json!({"app": slug})),
                ConnState::Failed => self.audit("composio.connect_failed", json!({"app": slug})),
                _ => {}
            }
        }
        self.sync_tools(slug).await;
        Ok(())
    }

    /// Start connecting `slug`: returns the Composio-hosted consent link.
    /// Opting in happens here (the owner clicked Connect).
    ///
    /// # Errors
    /// Invalid slug, not configured, Composio errors.
    pub async fn connect(&self, slug: &str) -> Result<Link> {
        if !valid_slug(slug) || !crate::surface::toolkit_allowed(slug) {
            return Err(ComposioError::Invalid("invalid app slug".into()));
        }
        let _guard = self.app_guard(slug).await;
        let client = self.client().await?;
        self.check_toolkit(&client, slug).await?;
        // A retry replaces only the account this installation stored for the app, and only
        // when it is unfinished or expired. An active account, a second account and an account
        // this installation did not create are never removed to make room for a new link.
        if let Ok(own) = self.bound_account(slug) {
            let accounts = self.note(client.list_accounts(&self.user_id(), Some(slug)).await)?;
            self.check_accounts(slug, &accounts)?;
            for account in accounts.iter().filter(|a| a.id == own) {
                if account.status == "ACTIVE" {
                    return Err(ComposioError::Invalid(
                        "the app is already connected".into(),
                    ));
                }
                self.note(client.delete_account(&own).await)?;
            }
        }
        let link = if self.linkage() == Linkage::Login {
            self.note(self.router_link(&client, slug).await)?
        } else {
            let cached = self.0.auth_configs.lock().get(slug).cloned();
            let auth_config = if let Some(id) = cached {
                id
            } else {
                let id = self.note(client.ensure_auth_config(slug).await)?;
                self.0
                    .auth_configs
                    .lock()
                    .insert(slug.to_owned(), id.clone());
                id
            };
            self.note(client.create_link(&auth_config, &self.user_id()).await)?
        };
        // The link is only ever handed to the owner's browser: whatever the client parsed, a
        // link off Composio's hosts is refused before anything is persisted as pending.
        if !crate::client::is_composio_https_url(&link.redirect_url) {
            return Err(ComposioError::Upstream(
                "Composio returned a connect link outside its own domain; it was discarded".into(),
            ));
        }
        self.save_state(|s| {
            let app = s.apps.entry(slug.to_owned()).or_default();
            app.enabled = true;
            app.account_id = Some(link.account_id.clone());
        })?;
        self.0
            .statuses
            .lock()
            .insert(slug.to_owned(), ConnState::Pending);
        self.0.account_labels.lock().remove(slug);
        self.unregister_app(slug);
        self.0
            .pending_since
            .lock()
            .insert(slug.to_owned(), Instant::now());
        Ok(link)
    }

    /// Refuse a slug Composio does not offer as an ordinary app.  The search
    /// can miss an app Praxi itself pins (the live slug spelling is not
    /// guaranteed), so for those a miss or a refused lookup is not final: the
    /// link request that follows names Composio's real reason.
    async fn check_toolkit(&self, client: &ComposioClient, slug: &str) -> Result<()> {
        let pinned = self.0.curated.is_popular(slug);
        match self.note(client.lookup_toolkit(slug).await) {
            Ok(ToolkitLookup::Found) => Ok(()),
            Ok(ToolkitLookup::Unsupported) => Err(ComposioError::Invalid(
                "unknown or unsupported app toolkit; connection refused".into(),
            )),
            Ok(ToolkitLookup::Missing) | Err(ComposioError::Upstream(_)) if pinned => Ok(()),
            Ok(ToolkitLookup::Missing) => Err(ComposioError::NotFound(format!(
                "Composio does not offer an app called `{slug}`"
            ))),
            Err(e) => Err(e),
        }
    }

    /// A connect link through the user Tool Router session; a stale session
    /// is replaced once.
    async fn router_link(&self, client: &ComposioClient, slug: &str) -> Result<Link> {
        for attempt in 0..2 {
            let cached = self.0.router_session.lock().clone();
            let session = if let Some(id) = cached {
                id
            } else {
                let id = client.create_router_session(&self.user_id()).await?;
                *self.0.router_session.lock() = Some(id.clone());
                id
            };
            match client.router_link(&session, slug).await {
                Ok(link) => return Ok(link),
                Err(ComposioError::Upstream(_)) if attempt == 0 => {
                    *self.0.router_session.lock() = None;
                }
                Err(e) => return Err(e),
            }
        }
        Err(ComposioError::Upstream(
            "could not create a connect link".into(),
        ))
    }

    /// Check the installation's account binding. For apps with a vetted probe,
    /// also run their fixed read-only call (see `probe`). Audit first.
    ///
    /// # Errors
    /// The app is not connected or Composio failed.
    pub async fn test_app(&self, slug: &str) -> Result<ProbeOutcome> {
        if !valid_slug(slug) || !crate::surface::toolkit_allowed(slug) {
            return Err(ComposioError::Invalid("invalid app slug".into()));
        }
        let _guard = self.app_guard(slug).await;
        let probe = probe_for(slug);
        if !self.0.state.lock().apps.contains_key(slug) {
            return Err(ComposioError::Invalid("the app is not connected".into()));
        }
        // The row is written before the first request to Composio: a test that
        // cannot be recorded is not sent.
        self.0
            .audit
            .record("composio.test", json!({"app": slug, "status": "started"}))
            .map_err(|_| {
                ComposioError::Internal("audit log unavailable; the test was not sent".into())
            })?;
        let mut out = ProbeOutcome::default();
        let result = async {
            self.refresh_connection(slug).await?;
            if self.0.statuses.lock().get(slug) != Some(&ConnState::Active) {
                return Err(ComposioError::Invalid("the app is not connected".into()));
            }
            let Some(probe) = probe.filter(|_| self.is_usable(slug)) else {
                out.account = self.0.account_labels.lock().get(slug).cloned();
                out.authorization_only = true;
                return Ok(());
            };
            let actions = self.discovered(slug).await?;
            let account = self.bound_account(slug)?;
            if let Some(step) = &probe.identity {
                match self.probe_step(&actions, step, &account).await {
                    Ok(Some(v)) => out.account = find_string(&v, probe.identity_keys),
                    // The sample below is the real proof; only an app that has
                    // nothing else must fail on this step.
                    Err(e) if probe.sample.is_none() => return Err(e),
                    Ok(None) | Err(_) => {}
                }
                if probe.sample.is_none() && out.account.is_none() {
                    return Err(ComposioError::Upstream(
                        "the app answered but did not name the account".into(),
                    ));
                }
            }
            if let Some(step) = &probe.sample {
                let v = self
                    .probe_step(&actions, step, &account)
                    .await?
                    .ok_or_else(|| {
                        ComposioError::Invalid("this app offers no read action to test with".into())
                    })?;
                out.items = sample_items(&v, probe.item_keys);
                if out.account.is_none() && slug == "googlecalendar" {
                    out.account = primary_calendar(&v);
                }
            }
            Ok(())
        }
        .await;
        match &result {
            Ok(()) => self.audit("composio.test", json!({"app": slug, "status": "ok"})),
            Err(_) => self.audit("composio.test", json!({"app": slug, "status": "error"})),
        }
        if matches!(result, Err(ComposioError::Unauthorized)) {
            self.audit("composio.key_rejected", json!({}));
        }
        result.map(|()| out)
    }

    /// One step of a probe; `Ok(None)` when the action does not exist.
    async fn probe_step(
        &self,
        actions: &[ActionInfo],
        step: &Step,
        account: &str,
    ) -> Result<Option<Value>> {
        let Some(action) = actions.iter().find(|a| a.tool_slug == step.action) else {
            return Ok(None);
        };
        let args = fit_args(&action.schema, &step.args);
        let data = self
            .client()
            .await?
            .execute(
                &action.tool_slug,
                &self.user_id(),
                &action.version,
                account,
                &args,
            )
            .await;
        self.note(data).map(Some)
    }

    /// Choose which bots may use an app: `None` = every bot, a list = only
    /// those bots (an empty list switches it off for all of them).
    ///
    /// # Errors
    /// Unknown app, a bot id that is not a bot id, or the state file cannot
    /// be written.
    pub async fn set_bots(&self, slug: &str, bots: Option<Vec<String>>) -> Result<AppView> {
        let _guard = self.known_app_guard(slug).await?;
        if let Some(list) = &bots
            && (list.len() > 1000 || list.iter().any(|b| uuid::Uuid::parse_str(b).is_err()))
        {
            return Err(ComposioError::Invalid("not a valid bot id".into()));
        }
        // Stored in the one spelling `bot_listed` compares with.
        let bots = bots.map(|l| {
            let mut l: Vec<String> = l
                .iter()
                .filter_map(|b| uuid::Uuid::parse_str(b).ok())
                .map(|u| u.hyphenated().to_string())
                .collect();
            l.sort();
            l.dedup();
            l
        });
        self.save_state(|st| {
            if let Some(app) = st.apps.get_mut(slug) {
                app.bots = bots;
            }
        })?;
        self.sync_tools(slug).await;
        self.audit("composio.bots", json!({"app": slug}));
        let cfg = self
            .0
            .state
            .lock()
            .apps
            .get(slug)
            .cloned()
            .unwrap_or_default();
        Ok(self.view(slug, &cfg))
    }

    /// Switch an app's tools on or off.
    ///
    /// # Errors
    /// Unknown app or the state file cannot be written.
    pub async fn set_enabled(&self, slug: &str, enabled: bool) -> Result<AppView> {
        let _guard = self.known_app_guard(slug).await?;
        self.save_state(|s| {
            if let Some(a) = s.apps.get_mut(slug) {
                a.enabled = enabled;
            }
        })?;
        self.sync_tools(slug).await;
        let cfg = self
            .0
            .state
            .lock()
            .apps
            .get(slug)
            .cloned()
            .unwrap_or_default();
        Ok(self.view(slug, &cfg))
    }

    /// Disconnect: delete the Composio-side account(s) when possible, always
    /// forget the app locally.  Returns whether the remote side was deleted.
    ///
    /// # Errors
    /// Unknown app or the state file cannot be written.
    pub async fn disconnect(&self, slug: &str) -> Result<bool> {
        let _guard = self.known_app_guard(slug).await?;
        let remote = async {
            let client = self.client().await?;
            let accounts = client.list_accounts(&self.user_id(), Some(slug)).await?;
            self.check_accounts(slug, &accounts)?;
            for a in accounts {
                client.delete_account(&a.id).await?;
            }
            Ok::<(), ComposioError>(())
        }
        .await
        .is_ok();
        self.save_state(|s| {
            s.apps.remove(slug);
        })?;
        self.0.statuses.lock().remove(slug);
        self.0.account_labels.lock().remove(slug);
        self.0.pending_since.lock().remove(slug);
        self.0.discovered.lock().remove(slug);
        self.unregister_app(slug);
        Ok(remote)
    }

    // --- discovery and tool registration --------------------------------

    pub(crate) async fn discovered(&self, slug: &str) -> Result<Arc<Vec<ActionInfo>>> {
        if !self.is_usable(slug) || !crate::surface::toolkit_allowed(slug) {
            return Err(ComposioError::Invalid(
                "app is not connected and enabled".into(),
            ));
        }
        self.toolkit_actions(slug).await
    }

    /// Catalog actions available to bots after the owner connects this app.
    ///
    /// # Errors
    /// Unsupported toolkit or failed discovery.
    pub async fn toolkit_actions(&self, slug: &str) -> Result<Arc<Vec<ActionInfo>>> {
        if !crate::surface::toolkit_allowed(slug) {
            return Err(ComposioError::Invalid("invalid app slug".into()));
        }
        if let Some((at, v)) = self.0.discovered.lock().get(slug)
            && at.elapsed() < DISCOVERY_TTL
        {
            return Ok(Arc::clone(v));
        }
        let remote = self.client().await?.list_tools(slug).await?;
        let mut seen = std::collections::BTreeSet::new();
        let actions: Vec<ActionInfo> = remote
            .into_iter()
            .filter_map(|t| action_from_remote(&self.0.curated, slug, t))
            .filter(|a| seen.insert(a.name.clone()))
            .collect();
        let v = Arc::new(actions);
        self.0
            .discovered
            .lock()
            .insert(slug.to_owned(), (Instant::now(), Arc::clone(&v)));
        Ok(v)
    }

    /// What the owner may preview of an app's actions, before or after connecting it: names and
    /// capped one-line descriptions, at most [`PREVIEW_ACTIONS`] of them. Reads the catalog only:
    /// nothing is executed, registered for bots or audited as a run.
    ///
    /// # Errors
    /// Unsupported toolkit or failed discovery.
    pub async fn preview_actions(&self, slug: &str) -> Result<Vec<ActionPreview>> {
        Ok(self
            .toolkit_actions(slug)
            .await?
            .iter()
            .take(PREVIEW_ACTIONS)
            .map(|a| ActionPreview {
                name: a.name.clone(),
                description: preview_line(
                    &(self.0.redact)(&a.description),
                    PREVIEW_DESCRIPTION_CHARS,
                ),
            })
            .collect())
    }

    /// Drop the cached action list of `slug` so the next use re-reads it (tests).
    #[cfg(test)]
    pub(crate) fn forget_discovery(&self, slug: &str) {
        self.0.discovered.lock().remove(slug);
    }

    /// The direct tools of an app: known read-only actions first, then
    /// Composio's order, at most `cap_per_toolkit`.
    fn select_direct(&self, actions: &[ActionInfo]) -> Vec<ActionInfo> {
        let (read, rest): (Vec<_>, Vec<_>) =
            actions.iter().cloned().partition(|a| a.class == "read");
        read.into_iter()
            .chain(rest)
            .take(self.0.curated.cap_per_toolkit)
            .collect()
    }

    fn unregister_app(&self, slug: &str) {
        if let Some(names) = self.0.registered.lock().remove(slug) {
            for n in names {
                self.0.registry.unregister(&n);
            }
        }
    }

    /// Make the registered tools match the app: present while it is active and
    /// enabled, gone otherwise.  A discovery failure leaves the app without
    /// direct tools (the search tool reports the error).
    pub(crate) async fn sync_tools(&self, slug: &str) {
        let want = self.is_usable(slug);
        if !want {
            self.unregister_app(slug);
            return;
        }
        let Ok(actions) = self.discovered(slug).await else {
            self.unregister_app(slug);
            return;
        };
        self.unregister_app(slug);
        let mut names = Vec::new();
        for a in self.select_direct(&actions) {
            let tool = ActionTool::new(self.clone(), slug, &a);
            let name = tool.spec_name().to_owned();
            if self.0.registry.register(Arc::new(tool)) {
                names.push(name);
            }
        }
        self.0.registered.lock().insert(slug.to_owned(), names);
    }

    /// Active and enabled.
    pub(crate) fn is_usable(&self, slug: &str) -> bool {
        self.configured()
            && self
                .0
                .state
                .lock()
                .apps
                .get(slug)
                .is_some_and(|a| a.enabled)
            && self.0.statuses.lock().get(slug) == Some(&ConnState::Active)
    }

    /// Apps `bot` may use right now.
    pub(crate) fn usable_apps(&self, bot: Option<&str>) -> Vec<String> {
        let st = self.0.state.lock();
        st.apps
            .iter()
            .filter(|(slug, cfg)| {
                crate::surface::toolkit_allowed(slug)
                    && cfg.enabled
                    && self.0.statuses.lock().get(slug.as_str()) == Some(&ConnState::Active)
                    && bot_listed(cfg.bots.as_deref(), bot)
            })
            .map(|(s, _)| s.clone())
            .collect()
    }

    pub(crate) fn app_visible_to(&self, slug: &str, bot: Option<&str>) -> bool {
        self.configured()
            && crate::surface::toolkit_allowed(slug)
            && self.0.state.lock().apps.get(slug).is_some_and(|c| {
                c.enabled
                    && self.0.statuses.lock().get(slug) == Some(&ConnState::Active)
                    && bot_listed(c.bots.as_deref(), bot)
            })
    }

    pub(crate) fn display_name(&self, slug: &str) -> String {
        self.0
            .curated
            .popular_name(slug)
            .map_or_else(|| slug.to_owned(), str::to_owned)
    }

    /// Actions of `app` matching `query` (all words, name or description).
    pub(crate) async fn find(
        &self,
        app: &str,
        query: &str,
        bot: Option<&str>,
    ) -> Result<Vec<(ActionInfo, bool)>> {
        if !self.app_visible_to(app, bot) {
            return Err(ComposioError::NotFound(format!(
                "app `{app}` is not connected and enabled for this bot"
            )));
        }
        let actions = self.discovered(app).await?;
        let direct: std::collections::BTreeSet<String> = self
            .select_direct(&actions)
            .into_iter()
            .map(|a| a.name)
            .collect();
        let words: Vec<String> = query
            .to_ascii_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut scored: Vec<(usize, &ActionInfo)> = actions
            .iter()
            .filter_map(|a| {
                let hay = format!("{} {}", a.name, a.description.to_ascii_lowercase());
                let hits = words.iter().filter(|w| hay.contains(w.as_str())).count();
                (words.is_empty() || hits == words.len()).then_some((hits, a))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(scored
            .into_iter()
            .take(8)
            .map(|(_, a)| (a.clone(), direct.contains(&a.name)))
            .collect())
    }

    /// Look one action up for a call (also used by the approval card).
    pub(crate) async fn action(
        &self,
        app: &str,
        name: &str,
        bot: Option<&str>,
    ) -> Result<ActionInfo> {
        if !self.app_visible_to(app, bot) {
            return Err(ComposioError::NotFound(format!(
                "app `{app}` is not connected and enabled for this bot"
            )));
        }
        self.discovered(app)
            .await?
            .iter()
            .find(|a| a.name == name)
            .cloned()
            .ok_or_else(|| {
                ComposioError::NotFound(format!(
                    "app `{app}` has no action `{name}`; use apps_find_action"
                ))
            })
    }

    /// Run one validated action.  Two audit rows: `started` before the call
    /// leaves, the final status after.  The row carries a hash of the
    /// arguments, never the arguments.
    pub(crate) async fn run(
        &self,
        app: &str,
        action: &ActionInfo,
        run_id: &str,
        bot: Option<&str>,
        args: &Value,
    ) -> Result<Value> {
        let prepared = self
            .0
            .prepared
            .lock()
            .remove(&approval_key(run_id, bot, app, &action.name, args))
            .ok_or_else(|| {
                ComposioError::Invalid("a fresh reviewable approval is required".into())
            })?;
        self.refresh(app).await?;
        self.0.discovered.lock().remove(app);
        let current = self.action(app, &action.name, bot).await?;
        let account = self.bound_account(app)?;
        if prepared.at.elapsed() > Duration::from_secs(600)
            || prepared.action != current
            || *action != current
            || prepared.account != account
        {
            return Err(ComposioError::Invalid(
                "account, version or schema changed; request approval again".into(),
            ));
        }
        let hash = format!("{:x}", Sha256::digest(args.to_string().as_bytes()));
        let row = |status: &str, bytes: usize| {
            json!({"app": app, "action": action.name, "class": action.class, "bot": bot,
                   "run": run_id, "status": status, "args_sha256": hash, "bytes": bytes})
        };
        self.0
            .audit
            .record("composio.call", row("started", 0))
            .map_err(|_| {
                ComposioError::Internal("audit log unavailable; the call was not sent".into())
            })?;

        let out = async {
            self.client()
                .await?
                .execute(
                    &action.tool_slug,
                    &self.user_id(),
                    &action.version,
                    &account,
                    args,
                )
                .await
        }
        .await;
        match &out {
            Ok(v) => self.audit("composio.call", row("ok", v.to_string().len())),
            Err(_) => self.audit("composio.call", row("error", 0)),
        }
        if matches!(out, Err(ComposioError::Unauthorized)) {
            self.audit("composio.key_rejected", json!({}));
        }
        out
    }

    fn check_accounts(&self, slug: &str, accounts: &[Account]) -> Result<()> {
        let bound = self.bound_account(slug)?;
        if accounts.len() > 1 || accounts.iter().any(|a| a.toolkit != slug || a.id != bound) {
            return Err(ComposioError::Invalid("ambiguous or foreign account; disconnect other accounts and reconnect the chosen account through this installation".into()));
        }
        Ok(())
    }

    fn bound_account(&self, app: &str) -> Result<String> {
        self.0
            .state
            .lock()
            .apps
            .get(app)
            .and_then(|c| c.account_id.clone())
            .ok_or_else(|| {
                ComposioError::Invalid("no installation account binding; reconnect this app".into())
            })
    }

    pub(crate) async fn prepare(
        &self,
        app: &str,
        action: &str,
        ctx: &tool_registry::ToolContext,
        args: &Value,
    ) -> Result<(ActionInfo, String)> {
        if !self.app_visible_to(app, ctx.bot_id.as_deref()) {
            return Err(ComposioError::NotFound(
                "app is not connected and enabled for this bot".into(),
            ));
        }
        self.refresh(app).await?;
        let a = self.action(app, action, ctx.bot_id.as_deref()).await?;
        crate::schema::validate(&a.schema, args).map_err(ComposioError::Invalid)?;
        let token = uuid::Uuid::new_v4().to_string();
        let mut pending = self.0.prepared.lock();
        pending.retain(|_, p| p.at.elapsed() < Duration::from_secs(600));
        if pending.len() >= 128 {
            return Err(ComposioError::Invalid(
                "too many pending app approvals".into(),
            ));
        }
        pending.insert(
            approval_key(&ctx.run_id, ctx.bot_id.as_deref(), app, action, args),
            Prepared {
                at: Instant::now(),
                token: token.clone(),
                account: self.bound_account(app)?,
                action: a.clone(),
            },
        );
        Ok((a, token))
    }

    pub(crate) fn close(
        &self,
        app: &str,
        action: &str,
        ctx: &tool_registry::ToolContext,
        args: &Value,
        token: Option<&str>,
    ) {
        let key = approval_key(&ctx.run_id, ctx.bot_id.as_deref(), app, action, args);
        let mut pending = self.0.prepared.lock();
        if pending
            .get(&key)
            .is_some_and(|p| Some(p.token.as_str()) == token)
        {
            pending.remove(&key);
        }
    }

    /// Names of the directly registered tools of `app` (tests, status).
    #[must_use]
    pub fn registered_tools(&self, app: &str) -> Vec<String> {
        self.0
            .registered
            .lock()
            .get(app)
            .cloned()
            .unwrap_or_default()
    }

    /// The logo of toolkit `slug`: from the disk cache, else fetched once from the URL the
    /// catalog carries (see [`crate::logo`] for every limit) and cached. `Ok(None)`: no logo is
    /// known for this app.
    ///
    /// # Errors
    /// The slug is malformed, or the download / validation failed.
    pub async fn logo(&self, slug: &str) -> Result<Option<crate::logo::Logo>> {
        if !valid_slug(slug) {
            return Err(ComposioError::Invalid("malformed app slug".into()));
        }
        let data = self
            .0
            .path
            .parent()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| ComposioError::Internal("no data directory".into()))?;
        if let Some(logo) = crate::logo::cached(&data, slug) {
            return Ok(Some(logo));
        }
        let url = self
            .0
            .catalog_file
            .lock()
            .as_ref()
            .and_then(|f| f.items.iter().find(|t| t.slug == slug))
            .and_then(|t| t.logo.clone());
        let Some(url) = url else {
            return Ok(None);
        };
        let logo = crate::logo::fetch(&url)
            .await
            .map_err(|_| ComposioError::Upstream("logo unavailable".into()))?;
        crate::logo::store(&data, slug, &logo);
        Ok(Some(logo))
    }

    /// Directory of `composio.toml` (tests).
    #[must_use]
    pub fn state_path(&self) -> &Path {
        &self.0.path
    }

    /// The configured bots map, for export/tests.
    #[must_use]
    pub fn bots_of(&self) -> BTreeMap<String, Option<Vec<String>>> {
        self.0
            .state
            .lock()
            .apps
            .iter()
            .map(|(k, v)| (k.clone(), v.bots.clone()))
            .collect()
    }
}

/// `None` allows every bot (and callers without one); a list allows only its
/// members; an empty list allows nobody.
pub(crate) fn bot_listed(bots: Option<&[String]>, bot: Option<&str>) -> bool {
    match bots {
        None => true,
        Some(list) => bot.is_some_and(|b| list.iter().any(|x| x == b)),
    }
}

fn tracing_warn(msg: &str) {
    eprintln!("warning: composio audit: {msg}");
}

fn action_from_remote(
    curated: &Curated,
    slug: &str,
    t: crate::client::RemoteTool,
) -> Option<ActionInfo> {
    if t.toolkit != slug || !crate::surface::action_allowed(slug, &t.slug) {
        return None;
    }
    Some(ActionInfo {
        class: display_class(curated, &t.slug),
        name: short_name(slug, &t.slug),
        description: t.description,
        schema: t.schema,
        tool_slug: t.slug,
        version: t.version,
    })
}

fn approval_key(run: &str, bot: Option<&str>, app: &str, action: &str, args: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([run, bot, app, action, args]).to_string().as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_toolkit_is_checked_even_when_the_action_prefix_matches() {
        let remote = crate::client::RemoteTool {
            slug: "GITHUB_READ".into(),
            toolkit: "gmail".into(),
            version: "20260901_00".into(),
            description: String::new(),
            schema: json!({"type":"object"}),
        };
        assert!(action_from_remote(&Curated::builtin(), "github", remote).is_none());
    }

    #[test]
    fn a_preview_line_is_plain_single_spaced_and_cut_with_a_mark() {
        assert_eq!(preview_line("  a\n\tb \r\n c  ", 160), "a b c");
        assert_eq!(
            preview_line("a\u{202e}b\u{200b}c\u{feff}d\u{2066}e", 160),
            "abcde"
        );
        assert_eq!(preview_line("abcdef", 6), "abcdef");
        assert_eq!(preview_line("abcdefg", 6), "abcdef…");
        assert_eq!(preview_line("ab cd ef", 3), "ab…");
        assert_eq!(preview_line("çğıöşü çğıöşü", 6), "çğıöşü…");
        assert_eq!(preview_line("\u{202e}\u{7}", 160), "");
    }

    #[test]
    fn bot_rules_match_the_mcp_ones() {
        assert!(bot_listed(None, None));
        assert!(!bot_listed(Some(&[]), Some("a")));
        assert!(!bot_listed(Some(&["a".into()]), None));
        assert!(bot_listed(Some(&["a".into()]), Some("a")));
    }

    #[test]
    fn slugs_are_restricted() {
        assert!(valid_slug("instagram"));
        assert!(valid_slug("microsoft_teams"));
        for bad in ["", "A", "a/b", "a b", "../x", "a?b"] {
            assert!(!valid_slug(bad), "{bad}");
        }
    }
}
