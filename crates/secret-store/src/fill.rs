//! Owner-stored *fill secrets*: credentials the fill tools may type for the
//! model without the model ever holding the value.
//!
//! The principle: a value is secret only if the OWNER stored it as one. The
//! model names a secret (`secret: "login/github"`) and the daemon resolves the
//! value itself, in memory, after the approval and after checking where it
//! would be typed against this secret's allow-list.
//!
//! * The value lives in the ordinary [`SecretStore`] under `fill/<name>`. No
//!   configuration (providers, MCP, `OpenAPI`, e-mail, channels) can reference a
//!   `fill/` name ([`crate::refuse_fill_namespace`]), and the fill tools can
//!   only ever build `fill/<name>` keys, so `composio/...`, `mcp/...` or a
//!   provider key can never be read through the `secret` parameter.
//! * The metadata (allowed browser origins, allowed desktop programs, standing
//!   grants) is kept OUTSIDE the secret value, in a small JSON file next to the
//!   data directory. It holds names and allow-lists only, never values.
//! * Only the local owner changes any of it (the daemon routes enforce that).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::secret::{FILL_PREFIX, SecretStore, SecretValue};

/// Largest accepted value.
pub const MAX_FILL_VALUE_BYTES: usize = 64 * 1024;
/// Kind of the only standing grant: typing a secret into a browser field.
pub const GRANT_KIND_BROWSER_FILL: &str = "browser_fill";

const MAX_NAME_BYTES: usize = 64;
const MAX_SECRETS: usize = 256;
const MAX_ORIGINS: usize = 32;
const MAX_PROGRAMS: usize = 32;
const MAX_GRANTS: usize = 512;
const MAX_ORIGIN_BYTES: usize = 256;
const MAX_PROGRAM_BYTES: usize = 200;
const META_VERSION: u32 = 1;

/// First path segments that name other parts of the store. A fill name can
/// never start with one (it could not collide anyway, because the store key is
/// `fill/<name>`), so a model or owner typo is refused with a clear reason.
const RESERVED_FIRST_SEGMENTS: &[&str] = &[
    "composio",
    "mcp",
    "provider",
    "providers",
    "email",
    "telegram",
    "openai",
    "anthropic",
    "gemini",
    "openrouter",
    "nvidia",
    "fill",
];

/// Why a fill-secret operation was refused. Messages carry names and origins,
/// never a value.
#[derive(Debug, Error)]
pub enum FillError {
    /// The secret name is not usable.
    #[error("invalid fill secret name: {0}")]
    InvalidName(String),
    /// The name starts with a reserved namespace.
    #[error("the name `{0}` is in a reserved namespace and cannot be used for a fill secret")]
    Reserved(String),
    /// An allow-list entry is not an exact origin / usable program.
    #[error("invalid allow-list entry: {0}")]
    InvalidEntry(String),
    /// Some other bad input (empty or oversized value, over a limit).
    #[error("{0}")]
    Invalid(String),
    /// No such fill secret.
    #[error("there is no fill secret named `{0}`")]
    Unknown(String),
    /// The secret exists but the owner did not allow it here. Carries the
    /// reason (shown to the model; contains no value).
    #[error("{0}")]
    NotAllowed(String),
    /// The metadata names the secret but the store has no value.
    #[error("the fill secret `{0}` has no stored value; ask the owner to set it again")]
    MissingValue(String),
    /// A secret with this name exists and the caller did not ask to replace it.
    #[error("a fill secret named `{0}` already exists")]
    Exists(String),
    /// Backend / file failure (no raw backend text: it may contain material).
    #[error("fill secrets are unavailable: {0}")]
    Unavailable(String),
}

/// One standing grant: "secret X on origin Y without asking".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillGrant {
    /// Opaque id used to revoke it.
    pub id: String,
    /// Fill secret name.
    pub secret: String,
    /// Exact canonical origin.
    pub origin: String,
    /// Always [`GRANT_KIND_BROWSER_FILL`] today.
    pub kind: String,
    /// Creation time, ms since the epoch.
    pub created_at_ms: u64,
}

/// What the owner sees about one secret. Never the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillSecretInfo {
    /// Name as the model writes it in `secret`.
    pub name: String,
    /// Exact origins the secret may be typed on.
    pub origins: Vec<String>,
    /// Verified desktop program identities the secret may be typed into.
    pub programs: Vec<String>,
    /// Whether the store holds a value for it.
    pub has_value: bool,
    /// Last change, ms since the epoch.
    pub updated_at_ms: u64,
}

/// Account facts of a secret a bot registered. Absent on older entries, which
/// stay usable by every bot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AccountMeta {
    site: String,
    login_id: String,
    kind: String,
    /// Bots that may use it. Empty = none.
    #[serde(default)]
    bot_ids: Vec<String>,
    created_by: String,
    #[serde(default)]
    email_account: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    created_at_ms: u64,
    /// When the password was first typed anywhere (`None` = never: a sign-up
    /// that failed before typing leaves the account in this state).
    #[serde(default)]
    first_used_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<AccountMeta>,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    programs: Vec<String>,
    #[serde(default)]
    updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    secrets: BTreeMap<String, Entry>,
    #[serde(default)]
    grants: Vec<FillGrant>,
}

/// What the owner and the bot may know about one site account. Never the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// Secret name (what `browser_fill` takes as `secret`).
    pub name: String,
    /// Canonical origin of the site.
    pub site: String,
    /// The login id (not secret).
    pub login_id: String,
    /// `password`.
    pub kind: String,
    /// `bot` (made by `account_register`) or `owner`.
    pub created_by: String,
    /// The mail identity used to open it, if any.
    pub email_account: Option<String>,
    /// Bots allowed to use it.
    pub bot_ids: Vec<String>,
    /// Short note the bot left.
    pub note: Option<String>,
    /// Whether the store holds the password.
    pub has_value: bool,
    /// Creation time, ms since the epoch.
    pub created_at_ms: u64,
    /// The password was never typed anywhere (a failed or abandoned sign-up).
    /// The owner can delete such an account, and its bot can discard it.
    pub unused: bool,
}

/// A new account for [`FillSecrets::register_account`].
#[derive(Debug, Clone)]
pub struct NewAccount<'a> {
    /// Secret name to store under.
    pub name: &'a str,
    /// Site origin, such as `https://example.com`.
    pub site: &'a str,
    /// The login id the form takes.
    pub login_id: &'a str,
    /// Mail identity used to open it.
    pub email_account: Option<&'a str>,
    /// The bot that owns the account.
    pub bot_id: &'a str,
    /// Short note.
    pub note: Option<&'a str>,
    /// Password length (clamped to [`PASSWORD_MIN_LEN`]..=[`PASSWORD_MAX_LEN`]).
    pub password_len: usize,
    /// Allow symbols in the password (some sites refuse them).
    pub symbols: bool,
}

/// Shortest generated password.
pub const PASSWORD_MIN_LEN: usize = 12;
/// Longest generated password.
pub const PASSWORD_MAX_LEN: usize = 64;

/// One uniformly random index below `n` from the OS CSPRNG (rejection sampling).
fn random_below(n: usize) -> Result<usize, FillError> {
    let limit = 65536 - (65536 % n);
    loop {
        let mut b = [0u8; 2];
        getrandom::fill(&mut b).map_err(|_| unavailable("no randomness available"))?;
        let v = usize::from(u16::from_le_bytes(b));
        if v < limit {
            return Ok(v % n);
        }
    }
}

/// A random password from the OS CSPRNG with at least one lower-case letter,
/// upper-case letter and digit (and one symbol when `symbols`). Look-alike
/// characters are left out.
///
/// # Errors
/// [`FillError::Unavailable`] when the OS has no randomness.
pub fn generate_password(len: usize, symbols: bool) -> Result<SecretValue, FillError> {
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const DIGIT: &[u8] = b"23456789";
    const SYMBOL: &[u8] = b"!#$%&*+-=?@^_";
    let len = len.clamp(PASSWORD_MIN_LEN, PASSWORD_MAX_LEN);
    let mut classes: Vec<&[u8]> = vec![LOWER, UPPER, DIGIT];
    if symbols {
        classes.push(SYMBOL);
    }
    let all: Vec<u8> = classes.iter().flat_map(|c| c.iter().copied()).collect();
    let mut out: Vec<u8> = Vec::with_capacity(len);
    for c in &classes {
        out.push(c[random_below(c.len())?]);
    }
    while out.len() < len {
        out.push(all[random_below(all.len())?]);
    }
    // Fisher-Yates, so the guaranteed characters are not always up front.
    for i in (1..out.len()).rev() {
        out.swap(i, random_below(i + 1)?);
    }
    // Every byte is ASCII.
    Ok(SecretValue::new(String::from_utf8_lossy(&out).into_owned()))
}

/// Called with every fill value as it is stored or read, so the production
/// redactor masks it wherever a page echoes it.
pub type ValueHook = Arc<dyn Fn(&str) + Send + Sync>;

/// The fill-secret service: metadata plus the one read path to the values.
pub struct FillSecrets {
    store: Arc<dyn SecretStore>,
    path: Option<PathBuf>,
    meta: Mutex<Meta>,
    hook: Option<ValueHook>,
}

impl std::fmt::Debug for FillSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FillSecrets(..)")
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn clean_origins(list: &[String]) -> Result<Vec<String>, FillError> {
    if list.len() > MAX_ORIGINS {
        return Err(FillError::Invalid("at most 32 origins".into()));
    }
    let set: BTreeSet<String> = list
        .iter()
        .map(|o| canonical_origin(o))
        .collect::<Result<_, _>>()?;
    Ok(set.into_iter().collect())
}

fn clean_programs(list: &[String]) -> Result<Vec<String>, FillError> {
    if list.len() > MAX_PROGRAMS {
        return Err(FillError::Invalid("at most 32 programs".into()));
    }
    let set: BTreeSet<String> = list
        .iter()
        .map(|p| clean_program(p))
        .collect::<Result<_, _>>()?;
    Ok(set.into_iter().collect())
}

fn check_value_size(v: &str) -> Result<(), FillError> {
    if v.is_empty() || v.len() > MAX_FILL_VALUE_BYTES {
        return Err(FillError::Invalid(
            "the value must be between 1 byte and 64 KiB".into(),
        ));
    }
    Ok(())
}

fn unavailable(what: &str) -> FillError {
    FillError::Unavailable(what.to_owned())
}

/// Where the metadata file lives inside `data_dir`.
#[must_use]
pub fn metadata_path(data_dir: &Path) -> PathBuf {
    data_dir.join("fill-secrets.json")
}

/// A short single-line rendering of owner- or page-supplied text for cards and
/// model-facing lists: control and bidirectional characters become spaces,
/// runs of whitespace collapse, and the result is capped at `cap` characters.
#[must_use]
pub fn sanitize_label(text: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in text.chars() {
        let hidden = c.is_control()
            || c.is_whitespace()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
        if hidden {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    if out.chars().count() > cap {
        let mut cut: String = out.chars().take(cap.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        out
    }
}

/// The canonical form of an exact origin: `scheme://host[:port]` with a
/// lowercase host and the default port dropped. No wildcards, no userinfo, no
/// path / query / fragment, `http` and `https` only. This is both how the
/// owner's allow-list entries are stored and how the live origin the browser
/// reports for a frame is compared.
///
/// # Errors
/// [`FillError::InvalidEntry`] when `raw` is not an exact web origin.
pub fn canonical_origin(raw: &str) -> Result<String, FillError> {
    let bad = |why: &str| FillError::InvalidEntry(format!("`{}`: {why}", sanitize_label(raw, 60)));
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_ORIGIN_BYTES {
        return Err(bad("an origin is 1 to 256 bytes"));
    }
    if raw.contains('*') {
        return Err(bad("wildcards are not supported"));
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("no whitespace"));
    }
    let url = Url::parse(raw).map_err(|_| bad("not a valid origin like https://example.com"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("only http and https origins"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("no user information"));
    }
    if url.query().is_some() || url.fragment().is_some() || !matches!(url.path(), "" | "/") {
        return Err(bad("an origin has no path, query or fragment"));
    }
    let origin = url.origin();
    if !origin.is_tuple() {
        return Err(bad("opaque origin"));
    }
    Ok(origin.ascii_serialization())
}

fn clean_program(raw: &str) -> Result<String, FillError> {
    let t = raw.trim();
    if t.is_empty() || t.len() > MAX_PROGRAM_BYTES || t.chars().any(char::is_control) {
        return Err(FillError::InvalidEntry(
            "a program identity is 1 to 200 printable bytes".into(),
        ));
    }
    Ok(t.to_owned())
}

/// Validate the name a model or owner writes (`login/github`).
///
/// # Errors
/// [`FillError::InvalidName`] or [`FillError::Reserved`].
pub fn validate_fill_name(name: &str) -> Result<(), FillError> {
    let bad = |why: &str| FillError::InvalidName(format!("`{}`: {why}", sanitize_label(name, 40)));
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(bad("1 to 64 bytes"));
    }
    let mut first = true;
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(bad("no empty, `.` or `..` path parts"));
        }
        if !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(bad("letters, digits, '.', '_', '-' and '/' only"));
        }
        if first {
            let lower = seg.to_ascii_lowercase();
            if RESERVED_FIRST_SEGMENTS.contains(&lower.as_str()) || lower.starts_with("__") {
                return Err(FillError::Reserved(name.to_owned()));
            }
            first = false;
        }
    }
    Ok(())
}

fn store_key(name: &str) -> String {
    format!("{FILL_PREFIX}{name}")
}

impl FillSecrets {
    /// Open (or start) the service over `store`, keeping metadata in
    /// `<data_dir>/fill-secrets.json`.
    ///
    /// # Errors
    /// [`FillError::Unavailable`] when the file exists but cannot be read or
    /// parsed (fail closed: no fill secret is usable until it is repaired).
    pub fn open(store: Arc<dyn SecretStore>, data_dir: &Path) -> Result<Self, FillError> {
        let path = metadata_path(data_dir);
        let meta = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Meta>(&bytes)
                .map_err(|_| unavailable("the metadata file is not valid JSON"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Meta::default(),
            Err(_) => return Err(unavailable("the metadata file cannot be read")),
        };
        if meta.version > META_VERSION {
            return Err(unavailable("the metadata file is from a newer version"));
        }
        Ok(Self {
            store,
            path: Some(path),
            meta: Mutex::new(meta),
            hook: None,
        })
    }

    /// A service that keeps its metadata in memory only (tests, ephemeral use).
    #[must_use]
    pub fn in_memory(store: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            path: None,
            meta: Mutex::new(Meta::default()),
            hook: None,
        }
    }

    /// Register every value with `hook` (the production redactor) as it is
    /// stored and as it is read.
    #[must_use]
    pub fn with_value_hook(mut self, hook: ValueHook) -> Self {
        self.hook = Some(hook);
        self
    }

    fn persist(&self, meta: &Meta) -> Result<(), FillError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut meta = meta.clone();
        meta.version = META_VERSION;
        let bytes = serde_json::to_vec_pretty(&meta)
            .map_err(|_| unavailable("cannot serialise the metadata"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|_| unavailable("cannot write the metadata file"))?;
        std::fs::rename(&tmp, path).map_err(|_| unavailable("cannot replace the metadata file"))
    }

    fn registered(&self, value: &str) {
        if let Some(hook) = &self.hook {
            hook(value);
        }
    }

    fn present_in_store(&self) -> Result<BTreeSet<String>, FillError> {
        self.store
            .list_names()
            .map(|n| n.into_iter().collect())
            .map_err(|_| unavailable("the secret store cannot be listed"))
    }

    fn info(name: &str, e: &Entry, present: &BTreeSet<String>) -> FillSecretInfo {
        FillSecretInfo {
            name: name.to_owned(),
            origins: e.origins.clone(),
            programs: e.programs.clone(),
            has_value: present.contains(&store_key(name)),
            updated_at_ms: e.updated_at_ms,
        }
    }

    /// Create or update a secret. `value` is write-only: required when the
    /// secret is new, optional (keep the stored value) on update. `origins` /
    /// `programs` replace the allow-lists when given.
    ///
    /// # Errors
    /// Invalid name / entries / value, a limit, or a backend failure.
    pub fn upsert(
        &self,
        name: &str,
        value: Option<&str>,
        origins: Option<&[String]>,
        programs: Option<&[String]>,
    ) -> Result<FillSecretInfo, FillError> {
        validate_fill_name(name)?;
        if let Some(v) = value
            && (v.is_empty() || v.len() > MAX_FILL_VALUE_BYTES)
        {
            return Err(FillError::Invalid(
                "the value must be between 1 byte and 64 KiB".into(),
            ));
        }
        let origins = origins.map(clean_origins).transpose()?;
        let programs = programs.map(clean_programs).transpose()?;

        let mut meta = self.meta.lock();
        let existed = meta.secrets.contains_key(name);
        if !existed {
            if value.is_none() {
                return Err(FillError::Invalid("a new fill secret needs a value".into()));
            }
            if meta.secrets.len() >= MAX_SECRETS {
                return Err(FillError::Invalid("too many fill secrets".into()));
            }
        }
        let before = meta.clone();
        if let Some(v) = value {
            self.store
                .set(&store_key(name), v)
                .map_err(|_| unavailable("the secret store rejected the write"))?;
            self.registered(v);
        }
        let entry = meta.secrets.entry(name.to_owned()).or_default();
        if let Some(o) = origins {
            entry.origins = o;
        }
        if let Some(p) = programs {
            entry.programs = p;
        }
        entry.updated_at_ms = now_ms();
        // A grant is only valid while its origin stays on the allow-list.
        let allowed: BTreeSet<String> = entry.origins.iter().cloned().collect();
        meta.grants
            .retain(|g| g.secret != name || allowed.contains(&g.origin));
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            if !existed && value.is_some() {
                let _ = self.store.delete(&store_key(name));
            }
            return Err(e);
        }
        let present = self.present_in_store()?;
        let entry = meta.secrets.get(name).cloned().unwrap_or_default();
        Ok(Self::info(name, &entry, &present))
    }

    /// Write several entries as one unit (a login: username and password):
    /// everything is validated first, and when any write or the metadata save
    /// fails the previous values and metadata are put back, so the vault never
    /// holds a mix of old and new. Returns whether an existing entry was replaced.
    ///
    /// # Errors
    /// [`FillError::Exists`] when a name exists and `replace` is false (checked
    /// under the same lock as the write); invalid input; a backend failure.
    pub fn upsert_all(
        &self,
        items: &[(String, String)],
        origins: &[String],
        programs: &[String],
        replace: bool,
    ) -> Result<bool, FillError> {
        for (n, v) in items {
            validate_fill_name(n)?;
            check_value_size(v)?;
        }
        let origins = clean_origins(origins)?;
        let programs = clean_programs(programs)?;
        let mut meta = self.meta.lock();
        let replaced = items.iter().any(|(n, _)| meta.secrets.contains_key(n));
        if replaced && !replace {
            let name = items
                .iter()
                .find(|(n, _)| meta.secrets.contains_key(n))
                .map_or("", |(n, _)| n.as_str());
            return Err(FillError::Exists(name.to_owned()));
        }
        let new = items
            .iter()
            .filter(|(n, _)| !meta.secrets.contains_key(n))
            .count();
        if meta.secrets.len() + new > MAX_SECRETS {
            return Err(FillError::Invalid("too many fill secrets".into()));
        }
        let mut previous = Vec::with_capacity(items.len());
        for (n, _) in items {
            previous.push(
                self.store
                    .get(&store_key(n))
                    .map_err(|_| unavailable("the secret store cannot be read"))?,
            );
        }
        let restore = |upto: usize| {
            for ((n, _), prev) in items.iter().zip(&previous).take(upto) {
                let _ = match prev {
                    Some(p) => self.store.set(&store_key(n), p.expose()),
                    None => self.store.delete(&store_key(n)).map(|_| ()),
                };
            }
        };
        for (i, (n, v)) in items.iter().enumerate() {
            if self.store.set(&store_key(n), v).is_err() {
                restore(i + 1);
                return Err(unavailable("the secret store rejected the write"));
            }
            self.registered(v);
        }
        let before = meta.clone();
        for (n, _) in items {
            let entry = meta.secrets.entry(n.clone()).or_default();
            entry.origins.clone_from(&origins);
            entry.programs.clone_from(&programs);
            entry.updated_at_ms = now_ms();
            let allowed: BTreeSet<String> = entry.origins.iter().cloned().collect();
            meta.grants
                .retain(|g| g.secret != *n || allowed.contains(&g.origin));
        }
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            restore(items.len());
            return Err(e);
        }
        Ok(replaced)
    }

    /// Create a site account: the DAEMON generates the password, stores it under
    /// `new.name` limited to `new.site` and to `new.bot_id`, and keeps the login
    /// facts for the Vault screen. The password is not returned; it goes only
    /// into the store and the redaction hook.
    ///
    /// # Errors
    /// Invalid name / site, the name already exists ([`FillError::Exists`]), a
    /// limit, or a backend failure. Nothing is left behind on failure.
    pub fn register_account(&self, new: &NewAccount<'_>) -> Result<AccountInfo, FillError> {
        validate_fill_name(new.name)?;
        let site = canonical_origin(new.site)?;
        let login = new.login_id.trim();
        if login.is_empty() || login.len() > 200 || login.chars().any(char::is_control) {
            return Err(FillError::Invalid(
                "the login id is 1 to 200 printable characters".into(),
            ));
        }
        if new.bot_id.is_empty() {
            return Err(FillError::Invalid(
                "an account belongs to a bot; there is no bot in this call".into(),
            ));
        }
        let password = generate_password(new.password_len, new.symbols)?;
        let mut meta = self.meta.lock();
        if meta.secrets.contains_key(new.name) {
            return Err(FillError::Exists(new.name.to_owned()));
        }
        if meta.secrets.len() >= MAX_SECRETS {
            return Err(FillError::Invalid("too many fill secrets".into()));
        }
        let before = meta.clone();
        self.store
            .set(&store_key(new.name), password.expose())
            .map_err(|_| unavailable("the secret store rejected the write"))?;
        self.registered(password.expose());
        let now = now_ms();
        let entry = Entry {
            account: Some(AccountMeta {
                site: site.clone(),
                login_id: login.to_owned(),
                kind: "password".into(),
                bot_ids: vec![new.bot_id.to_owned()],
                created_by: "bot".into(),
                email_account: new.email_account.map(|e| sanitize_label(e, 120)),
                note: new.note.map(|n| sanitize_label(n, 200)),
                created_at_ms: now,
                first_used_at_ms: None,
            }),
            origins: vec![site],
            programs: Vec::new(),
            updated_at_ms: now,
        };
        meta.secrets.insert(new.name.to_owned(), entry.clone());
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            let _ = self.store.delete(&store_key(new.name));
            return Err(e);
        }
        let present = self.present_in_store()?;
        Self::account_info(new.name, &entry, &present)
            .ok_or_else(|| unavailable("the account was not recorded"))
    }

    fn account_info(name: &str, e: &Entry, present: &BTreeSet<String>) -> Option<AccountInfo> {
        let a = e.account.as_ref()?;
        Some(AccountInfo {
            name: name.to_owned(),
            site: a.site.clone(),
            login_id: a.login_id.clone(),
            kind: a.kind.clone(),
            created_by: a.created_by.clone(),
            email_account: a.email_account.clone(),
            bot_ids: a.bot_ids.clone(),
            note: a.note.clone(),
            has_value: present.contains(&store_key(name)),
            created_at_ms: a.created_at_ms,
            unused: a.first_used_at_ms.is_none(),
        })
    }

    /// Record that the password of account `name` was typed. Idempotent; a no-op
    /// for secrets without account facts.
    ///
    /// # Errors
    /// A write failure.
    pub fn mark_used(&self, name: &str) -> Result<(), FillError> {
        let mut meta = self.meta.lock();
        let before = meta.clone();
        let Some(a) = meta
            .secrets
            .get_mut(name)
            .and_then(|e| e.account.as_mut())
            .filter(|a| a.first_used_at_ms.is_none())
        else {
            return Ok(());
        };
        a.first_used_at_ms = Some(now_ms());
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            return Err(e);
        }
        Ok(())
    }

    /// Delete an account its own bot made, with its password and grants (a
    /// failed sign-up must not block the retry for ever). The owner's own
    /// secrets and other bots' accounts are refused.
    ///
    /// # Errors
    /// Unknown name, not this bot's bot-made account, or a backend failure.
    pub fn discard_account(&self, name: &str, bot: Option<&str>) -> Result<(), FillError> {
        validate_fill_name(name)?;
        {
            let meta = self.meta.lock();
            let e = meta
                .secrets
                .get(name)
                .ok_or_else(|| FillError::Unknown(name.to_owned()))?;
            Self::scope_check(name, e, bot)?;
            if e.account.as_ref().is_none_or(|a| a.created_by != "bot") {
                return Err(FillError::NotAllowed(format!(
                    "the secret `{}` was not made by a bot; only the owner can remove it",
                    sanitize_label(name, 80)
                )));
            }
        }
        self.delete(name).map(|_| ())
    }

    /// Whether `bot` may use the secret. Entries without account facts (the
    /// owner's own secrets) stay open to every bot as before; an account is open
    /// only to the bots listed on it, never to a caller without a bot id.
    fn bot_may_use(e: &Entry, bot: Option<&str>) -> bool {
        match &e.account {
            None => true,
            Some(a) => bot.is_some_and(|b| a.bot_ids.iter().any(|x| x == b)),
        }
    }

    /// Refuse use of a bot-scoped account by a bot that does not own it.
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] (names the secret, nothing else).
    pub fn check_bot(&self, name: &str, bot: Option<&str>) -> Result<(), FillError> {
        let meta = self.meta.lock();
        match meta.secrets.get(name) {
            Some(e) => Self::scope_check(name, e, bot),
            None => Ok(()),
        }
    }

    fn scope_check(name: &str, e: &Entry, bot: Option<&str>) -> Result<(), FillError> {
        if Self::bot_may_use(e, bot) {
            Ok(())
        } else {
            Err(FillError::NotAllowed(format!(
                "the secret `{}` belongs to another bot",
                sanitize_label(name, 80)
            )))
        }
    }

    /// Secrets `bot` may use (what `secrets_list` shows it).
    ///
    /// # Errors
    /// The store cannot be listed.
    pub fn list_for_bot(&self, bot: Option<&str>) -> Result<Vec<FillSecretInfo>, FillError> {
        let present = self.present_in_store()?;
        Ok(self
            .meta
            .lock()
            .secrets
            .iter()
            .filter(|(_, e)| Self::bot_may_use(e, bot))
            .map(|(n, e)| Self::info(n, e, &present))
            .collect())
    }

    /// The site accounts `bot` may use (`bot = None`: every account, for the owner).
    ///
    /// # Errors
    /// The store cannot be listed.
    pub fn accounts(&self, bot: Option<&str>) -> Result<Vec<AccountInfo>, FillError> {
        let present = self.present_in_store()?;
        Ok(self
            .meta
            .lock()
            .secrets
            .iter()
            .filter(|(_, e)| bot.is_none() || Self::bot_may_use(e, bot))
            .filter_map(|(n, e)| Self::account_info(n, e, &present))
            .collect())
    }

    /// Every secret with its allow-lists. Never reads a value.
    ///
    /// # Errors
    /// The store cannot be listed.
    pub fn list(&self) -> Result<Vec<FillSecretInfo>, FillError> {
        let present = self.present_in_store()?;
        Ok(self
            .meta
            .lock()
            .secrets
            .iter()
            .map(|(n, e)| Self::info(n, e, &present))
            .collect())
    }

    /// Delete a secret, its value and its grants. Returns whether it existed.
    ///
    /// # Errors
    /// Backend failures.
    pub fn delete(&self, name: &str) -> Result<bool, FillError> {
        validate_fill_name(name)?;
        let mut meta = self.meta.lock();
        let before = meta.clone();
        let existed = meta.secrets.remove(name).is_some();
        meta.grants.retain(|g| g.secret != name);
        if existed && let Err(e) = self.persist(&meta) {
            *meta = before;
            return Err(e);
        }
        self.store
            .delete(&store_key(name))
            .map_err(|_| unavailable("the secret store rejected the delete"))?;
        Ok(existed)
    }

    /// Create a standing grant "secret `secret` on `origin` without asking".
    /// The origin must already be on the secret's allow-list.
    ///
    /// # Errors
    /// Unknown secret, origin not on its allow-list, a limit, or a write failure.
    pub fn grant_create(&self, secret: &str, origin: &str) -> Result<FillGrant, FillError> {
        validate_fill_name(secret)?;
        let origin = canonical_origin(origin)?;
        let mut meta = self.meta.lock();
        let entry = meta
            .secrets
            .get(secret)
            .ok_or_else(|| FillError::Unknown(secret.to_owned()))?;
        if !entry.origins.contains(&origin) {
            return Err(FillError::Invalid(
                "the origin is not on this secret's allowed list".into(),
            ));
        }
        if let Some(existing) = meta
            .grants
            .iter()
            .find(|g| g.secret == secret && g.origin == origin && g.kind == GRANT_KIND_BROWSER_FILL)
        {
            return Ok(existing.clone());
        }
        if meta.grants.len() >= MAX_GRANTS {
            return Err(FillError::Invalid("too many standing grants".into()));
        }
        let grant = FillGrant {
            id: uuid::Uuid::now_v7().to_string(),
            secret: secret.to_owned(),
            origin,
            kind: GRANT_KIND_BROWSER_FILL.to_owned(),
            created_at_ms: now_ms(),
        };
        let before = meta.clone();
        meta.grants.push(grant.clone());
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            return Err(e);
        }
        Ok(grant)
    }

    /// All standing grants.
    #[must_use]
    pub fn grants(&self) -> Vec<FillGrant> {
        self.meta.lock().grants.clone()
    }

    /// Revoke a grant by id. Returns whether it existed.
    ///
    /// # Errors
    /// A write failure.
    pub fn grant_revoke(&self, id: &str) -> Result<bool, FillError> {
        let mut meta = self.meta.lock();
        let before = meta.clone();
        let n = meta.grants.len();
        meta.grants.retain(|g| g.id != id);
        let removed = meta.grants.len() != n;
        if removed && let Err(e) = self.persist(&meta) {
            *meta = before;
            return Err(e);
        }
        Ok(removed)
    }

    /// [`Self::check_browser`] for a field in a frame of a page: BOTH the
    /// frame origin and the top-level page origin must be allowed, so an
    /// allowed site embedded in (or embedding) another origin is refused. A
    /// standing grant applies only when the two origins are the same.
    ///
    /// # Errors
    /// As [`Self::check_browser`], for either origin.
    pub fn check_browser_in(
        &self,
        name: &str,
        frame_origin: Option<&str>,
        top_origin: Option<&str>,
        bot: Option<&str>,
    ) -> Result<Option<String>, FillError> {
        let grant = self.check_browser(name, frame_origin, bot)?;
        self.check_browser(name, top_origin, bot)?;
        let same = matches!(
            (frame_origin.map(canonical_origin), top_origin.map(canonical_origin)),
            (Some(Ok(a)), Some(Ok(b))) if a == b
        );
        Ok(if same { grant } else { None })
    }

    /// Whether the secret may be typed on the page origin `live_origin` (the
    /// origin of the FRAME that holds the receiving field, as the browser
    /// reports it). `Ok(None)` = allowed, ask the owner; `Ok(Some(id))` = a
    /// standing grant that matches this exact secret, origin and kind covers it.
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] / [`FillError::Unknown`]: refuse before any
    /// approval is offered. `bot` is the acting bot; a bot-scoped account is
    /// refused for any other bot and for `None` (see [`Self::check_bot`]).
    pub fn check_browser(
        &self,
        name: &str,
        live_origin: Option<&str>,
        bot: Option<&str>,
    ) -> Result<Option<String>, FillError> {
        validate_fill_name(name)?;
        let meta = self.meta.lock();
        let entry = meta
            .secrets
            .get(name)
            .ok_or_else(|| FillError::Unknown(name.to_owned()))?;
        Self::scope_check(name, entry, bot)?;
        let origin = live_origin
            .and_then(|o| canonical_origin(o).ok())
            .ok_or_else(|| {
                FillError::NotAllowed(format!(
                    "refused: the page frame that holds this field has no verifiable web origin, so `{name}` is not typed there"
                ))
            })?;
        if !entry.origins.contains(&origin) {
            return Err(FillError::NotAllowed(format!(
                "refused: `{name}` is not allowed on {origin}; only the owner can add an origin to it"
            )));
        }
        Ok(meta
            .grants
            .iter()
            .find(|g| g.secret == name && g.origin == origin && g.kind == GRANT_KIND_BROWSER_FILL)
            .map(|g| g.id.clone()))
    }

    /// Whether the secret may be typed into the desktop program with verified
    /// identity `program`. No verified identity (the backend does not supply
    /// one) is always refused; there is never a standing grant for a desktop
    /// program.
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] / [`FillError::Unknown`].
    pub fn check_desktop(
        &self,
        name: &str,
        program: Option<&str>,
        bot: Option<&str>,
    ) -> Result<(), FillError> {
        validate_fill_name(name)?;
        let meta = self.meta.lock();
        let entry = meta
            .secrets
            .get(name)
            .ok_or_else(|| FillError::Unknown(name.to_owned()))?;
        Self::scope_check(name, entry, bot)?;
        let Some(program) = program else {
            return Err(FillError::NotAllowed(format!(
                "refused: this desktop backend cannot verify which program owns the field, so `{name}` is not typed there"
            )));
        };
        if entry.programs.iter().any(|p| p == program) {
            Ok(())
        } else {
            Err(FillError::NotAllowed(format!(
                "refused: `{name}` is not allowed in this program; only the owner can add one to it"
            )))
        }
    }

    /// The only read path to a value: `fill/<name>`. Call after the approval,
    /// hold the result in memory only. `bot` is the acting bot: a bot-scoped
    /// account is refused for any other bot and for `None`, so no caller can
    /// read or type a secret scoped to another bot. Owner-made secrets without
    /// account facts stay open to every caller.
    ///
    /// # Errors
    /// Unknown name, another bot's account, no stored value, or a backend failure.
    pub fn resolve_value_for(
        &self,
        name: &str,
        bot: Option<&str>,
    ) -> Result<SecretValue, FillError> {
        validate_fill_name(name)?;
        match self.meta.lock().secrets.get(name) {
            None => return Err(FillError::Unknown(name.to_owned())),
            Some(e) => Self::scope_check(name, e, bot)?,
        }
        let value = self
            .store
            .get(&store_key(name))
            .map_err(|_| unavailable("the secret store cannot be read"))?
            .ok_or_else(|| FillError::MissingValue(name.to_owned()))?;
        self.registered(value.expose());
        Ok(value)
    }

    /// Whether a secret with this name is registered (no value read).
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.meta.lock().secrets.contains_key(name)
    }

    /// Every stored fill VALUE, for a redactor (the production redactor already
    /// covers the whole store; this is for callers that hold only this service).
    ///
    /// # Errors
    /// Backend failures.
    pub fn values_for_redaction(&self) -> Result<Vec<String>, FillError> {
        let names: Vec<String> = self.meta.lock().secrets.keys().cloned().collect();
        let mut out = Vec::new();
        for n in names {
            if let Some(v) = self
                .store
                .get(&store_key(&n))
                .map_err(|_| unavailable("the secret store cannot be read"))?
            {
                out.push(v.expose().to_owned());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn svc() -> (Arc<MemoryStore>, FillSecrets) {
        let store = Arc::new(MemoryStore::new());
        let s = FillSecrets::in_memory(store.clone());
        (store, s)
    }

    fn o(s: &str) -> Vec<String> {
        vec![s.to_owned()]
    }

    /// A store whose second write fails: models a keychain error mid-login.
    struct FailSecondSet(MemoryStore, AtomicUsize);
    impl SecretStore for FailSecondSet {
        fn get(&self, n: &str) -> Result<Option<crate::SecretValue>, crate::SecretError> {
            self.0.get(n)
        }
        fn set(&self, n: &str, v: &str) -> Result<(), crate::SecretError> {
            if self.1.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(crate::SecretError::Keyring("boom".into()));
            }
            self.0.set(n, v)
        }
        fn delete(&self, n: &str) -> Result<bool, crate::SecretError> {
            self.0.delete(n)
        }
        fn list_names(&self) -> Result<Vec<String>, crate::SecretError> {
            self.0.list_names()
        }
    }

    #[test]
    fn upsert_all_restores_old_values_when_a_later_write_fails() {
        let store = Arc::new(FailSecondSet(MemoryStore::new(), AtomicUsize::new(100)));
        let s = FillSecrets::in_memory(store.clone());
        let pair = |u: &str, p: &str| {
            vec![
                ("login/x/username".to_owned(), u.to_owned()),
                ("login/x/password".to_owned(), p.to_owned()),
            ]
        };
        s.upsert_all(&pair("old-user", "old-pass"), &[], &[], false)
            .unwrap();
        assert!(matches!(
            s.upsert_all(&pair("a", "b"), &[], &[], false),
            Err(FillError::Exists(_))
        ));
        store.1.store(0, Ordering::SeqCst); // the next write is fine, the one after fails
        assert!(
            s.upsert_all(&pair("new-user", "new-pass"), &[], &[], true)
                .is_err()
        );
        let get = |n: &str| store.0.get(n).unwrap().unwrap().expose().to_owned();
        assert_eq!(get("fill/login/x/username"), "old-user");
        assert_eq!(get("fill/login/x/password"), "old-pass");
    }

    #[test]
    fn origins_are_exact_and_canonical() {
        assert_eq!(
            canonical_origin("HTTPS://GitHub.com:443/").unwrap(),
            "https://github.com"
        );
        assert_eq!(
            canonical_origin("http://localhost:8080").unwrap(),
            "http://localhost:8080"
        );
        for bad in [
            "",
            "github.com",
            "https://*.github.com",
            "https://github.com/login",
            "https://github.com?x=1",
            "https://user@github.com",
            "ftp://github.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://a b.com",
            "null",
        ] {
            assert!(canonical_origin(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn reserved_and_malformed_names_are_refused() {
        for bad in [
            "composio/gmail",
            "mcp/x/oauth",
            "MCP/x",
            "fill/x",
            "provider/openai",
            "__x",
            "",
            "a//b",
            "../x",
            "a/..",
            "a b",
            "a:b",
            "/a",
            "a/",
        ] {
            assert!(validate_fill_name(bad).is_err(), "{bad:?} must be refused");
        }
        for ok in ["login/github", "card.visa-cvc", "a", "work/vpn_1"] {
            validate_fill_name(ok).unwrap();
        }
    }

    #[test]
    fn value_is_write_only_and_lives_under_the_fill_namespace() {
        let (store, s) = svc();
        let info = s
            .upsert(
                "login/github",
                Some("hunter2-correct"),
                Some(&o("https://github.com")),
                None,
            )
            .unwrap();
        assert!(info.has_value);
        assert_eq!(store.list_names().unwrap(), vec!["fill/login/github"]);
        let listing = serde_json::to_string(&s.list().unwrap()).unwrap();
        assert!(!listing.contains("hunter2-correct"));
        assert_eq!(
            s.resolve_value_for("login/github", None).unwrap().expose(),
            "hunter2-correct"
        );
        // Update keeps the value when none is given.
        s.upsert("login/github", None, Some(&o("https://gitlab.com")), None)
            .unwrap();
        assert_eq!(
            s.resolve_value_for("login/github", None).unwrap().expose(),
            "hunter2-correct"
        );
    }

    #[test]
    fn a_name_outside_the_registry_never_reaches_other_store_entries() {
        let (store, s) = svc();
        store.set("composio/gmail", "composio-token-123").unwrap();
        for name in ["composio/gmail", "fill/../composio/gmail", "x"] {
            assert!(s.resolve_value_for(name, None).is_err(), "{name}");
        }
        // Even a registered name only ever reads `fill/<name>`.
        s.upsert("work/a", Some("a-value-123"), None, None).unwrap();
        assert_eq!(
            s.resolve_value_for("work/a", None).unwrap().expose(),
            "a-value-123"
        );
    }

    #[test]
    fn browser_check_is_exact_and_fails_closed() {
        let (_s, s) = svc();
        s.upsert(
            "login/github",
            Some("v-1234567"),
            Some(&o("https://github.com")),
            None,
        )
        .unwrap();
        assert!(
            s.check_browser("login/github", Some("https://github.com"), None)
                .unwrap()
                .is_none()
        );
        for live in [
            Some("https://evil.example"),
            Some("http://github.com"),
            Some("https://github.com:8443"),
            Some("https://github.com.evil.example"),
            Some("null"),
            Some(""),
            None,
        ] {
            assert!(
                matches!(
                    s.check_browser("login/github", live, None),
                    Err(FillError::NotAllowed(_))
                ),
                "{live:?}"
            );
        }
        assert!(matches!(
            s.check_browser("login/other", Some("https://github.com"), None),
            Err(FillError::Unknown(_))
        ));
    }

    #[test]
    fn a_standing_grant_matches_only_its_secret_and_origin() {
        let (_s, s) = svc();
        s.upsert(
            "a/one",
            Some("value-one-1"),
            Some(&["https://one.test".into(), "https://two.test".into()]),
            None,
        )
        .unwrap();
        s.upsert(
            "a/two",
            Some("value-two-2"),
            Some(&["https://one.test".into()]),
            None,
        )
        .unwrap();
        // The origin must already be allowed.
        assert!(s.grant_create("a/one", "https://other.test").is_err());
        let g = s.grant_create("a/one", "https://one.test").unwrap();
        assert_eq!(
            s.check_browser("a/one", Some("https://one.test"), None)
                .unwrap(),
            Some(g.id.clone())
        );
        assert_eq!(
            s.check_browser("a/one", Some("https://two.test"), None)
                .unwrap(),
            None
        );
        assert_eq!(
            s.check_browser("a/two", Some("https://one.test"), None)
                .unwrap(),
            None
        );
        // Dropping the origin from the allow-list drops its grant.
        s.upsert("a/one", None, Some(&["https://two.test".into()]), None)
            .unwrap();
        assert_eq!(s.grants().len(), 0);
        let g = s.grant_create("a/one", "https://two.test").unwrap();
        assert!(s.grant_revoke(&g.id).unwrap());
        assert!(!s.grant_revoke(&g.id).unwrap());
        assert_eq!(
            s.check_browser("a/one", Some("https://two.test"), None)
                .unwrap(),
            None
        );
        // Delete removes grants too.
        let g = s.grant_create("a/one", "https://two.test").unwrap();
        assert!(s.delete("a/one").unwrap());
        assert!(!s.grants().iter().any(|x| x.id == g.id));
    }

    #[test]
    fn desktop_needs_a_verified_identity_on_the_list() {
        let (_s, s) = svc();
        s.upsert(
            "app/vpn",
            Some("vpn-pass-123"),
            None,
            Some(&["win:sha256:abc".into()]),
        )
        .unwrap();
        s.check_desktop("app/vpn", Some("win:sha256:abc"), None)
            .unwrap();
        assert!(
            s.check_desktop("app/vpn", Some("win:sha256:other"), None)
                .is_err()
        );
        assert!(s.check_desktop("app/vpn", None, None).is_err());
    }

    #[test]
    fn metadata_survives_a_restart_and_holds_no_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        {
            let s = FillSecrets::open(store.clone(), dir.path()).unwrap();
            s.upsert(
                "login/x",
                Some("never-on-disk-123"),
                Some(&o("https://x.test")),
                None,
            )
            .unwrap();
            s.grant_create("login/x", "https://x.test").unwrap();
        }
        let raw = std::fs::read_to_string(metadata_path(dir.path())).unwrap();
        assert!(!raw.contains("never-on-disk-123"));
        assert!(raw.contains("login/x"));
        let s = FillSecrets::open(store, dir.path()).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert_eq!(s.grants().len(), 1);
        // A corrupt file fails closed.
        std::fs::write(metadata_path(dir.path()), b"{not json").unwrap();
        assert!(FillSecrets::open(Arc::new(MemoryStore::new()), dir.path()).is_err());
    }

    fn registered_account(s: &FillSecrets) -> &'static str {
        let name = "login/example.com-aaaa1111";
        s.register_account(&NewAccount {
            name,
            site: "https://example.com",
            login_id: "me",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
        name
    }

    #[test]
    fn the_bot_scope_is_enforced_inside_every_read_and_check() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let name = registered_account(&s);
        // The owner may later allow a program; another bot still must not use it.
        s.upsert(name, None, None, Some(&["win:sha256:abc".to_owned()]))
            .unwrap();
        let site = Some("https://example.com");
        for bot in [Some("bot-b"), None] {
            let errs = [
                s.resolve_value_for(name, bot).map(|_| ()).unwrap_err(),
                s.check_browser(name, site, bot).map(|_| ()).unwrap_err(),
                s.check_browser_in(name, site, site, bot)
                    .map(|_| ())
                    .unwrap_err(),
                s.check_desktop(name, Some("win:sha256:abc"), bot)
                    .unwrap_err(),
            ];
            for e in errs {
                assert!(matches!(e, FillError::NotAllowed(_)), "{e}");
                let text = e.to_string();
                let pw = s.resolve_value_for(name, Some("bot-a")).unwrap();
                assert!(!text.contains(pw.expose()));
            }
        }
        assert!(s.resolve_value_for(name, Some("bot-a")).is_ok());
        assert!(s.check_browser(name, site, Some("bot-a")).is_ok());
        assert!(
            s.check_desktop(name, Some("win:sha256:abc"), Some("bot-a"))
                .is_ok()
        );
    }

    #[test]
    fn owner_made_secrets_stay_global() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        s.upsert(
            "login/owner",
            Some("owner-value-123"),
            Some(&["https://example.com".to_owned()]),
            Some(&["win:sha256:abc".to_owned()]),
        )
        .unwrap();
        for bot in [Some("bot-a"), Some("bot-b"), None] {
            assert!(s.resolve_value_for("login/owner", bot).is_ok());
            assert!(
                s.check_browser("login/owner", Some("https://example.com"), bot)
                    .is_ok()
            );
            assert!(
                s.check_desktop("login/owner", Some("win:sha256:abc"), bot)
                    .is_ok()
            );
        }
    }

    #[test]
    fn the_value_hook_sees_stored_and_resolved_values() {
        let n = Arc::new(AtomicUsize::new(0));
        let seen = n.clone();
        let store = Arc::new(MemoryStore::new());
        let s = FillSecrets::in_memory(store).with_value_hook(Arc::new(move |v| {
            assert_eq!(v, "hooked-value-1");
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        s.upsert("h/one", Some("hooked-value-1"), None, None)
            .unwrap();
        s.resolve_value_for("h/one", None).unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn labels_are_single_line_and_capped() {
        assert_eq!(sanitize_label("a\nb\u{202e}c   d", 20), "a b c d");
        let long = "x".repeat(100);
        assert_eq!(sanitize_label(&long, 10).chars().count(), 10);
    }

    #[test]
    fn config_cannot_reference_the_fill_namespace() {
        use crate::{SecretError, SecretRef, resolve};
        let store = MemoryStore::new();
        store.set("fill/login/x", "some-secret-value").unwrap();
        assert!(matches!(
            SecretRef::new("fill/login/x"),
            Err(SecretError::Reserved(_))
        ));
        assert!(matches!(
            SecretRef::new("FILL/login/x"),
            Err(SecretError::Reserved(_))
        ));
        // A reference that came from deserialisation skips `new`.
        let r: SecretRef = serde_json::from_str("\"fill/login/x\"").unwrap();
        assert!(matches!(resolve(&store, &r), Err(SecretError::Reserved(_))));
    }
}
