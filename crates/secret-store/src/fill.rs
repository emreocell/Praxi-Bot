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
use crate::totp::TotpSpec;

/// Largest accepted value.
pub const MAX_FILL_VALUE_BYTES: usize = 64 * 1024;
/// Kind of the only standing grant: typing a secret into a browser field.
pub const GRANT_KIND_BROWSER_FILL: &str = "browser_fill";

const MAX_NAME_BYTES: usize = 64;
// A bot signing up on many sites keeps one entry per account; 1024 leaves room for a long
// registration task while the file and keychain stay small.
const MAX_SECRETS: usize = 1024;
/// Most entries one bot may make for itself (its own accounts and captured values). A bot
/// that loops on sign-ups cannot fill the shared store for the others; `account_discard`
/// frees room.
pub const MAX_BOT_OWN_SECRETS: usize = 256;
/// Longest value `capture_secret` stores (an API key or token, not a document).
pub const MAX_CAPTURED_BYTES: usize = 4096;
const MAX_ORIGINS: usize = 32;
const MAX_PROGRAMS: usize = 32;
const MAX_BOTS: usize = 64;
const MAX_GRANTS: usize = 512;
const MAX_ORIGIN_BYTES: usize = 256;
const MAX_PROGRAM_BYTES: usize = 200;
const MAX_BOT_ID_BYTES: usize = 64;
const MAX_SITE_NAME_CHARS: usize = 80;
const MAX_LOGIN_ID_CHARS: usize = 160;
const MAX_NOTE_CHARS: usize = 1024;
const META_VERSION: u32 = 2;

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

/// Kind of Vault entry (for grouping in the UI; never affects security boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultKind {
    /// A website or application login (username/email + password).
    #[default]
    Login,
    /// Any other owner-stored fill secret (API key, card field, PIN, etc.).
    Other,
}

/// What the owner sees about one secret. Never the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillSecretInfo {
    /// Name as the model writes it in `secret`.
    pub name: String,
    /// Category (`login` or `other`).
    pub kind: VaultKind,
    /// Human-friendly site or service label (never secret).
    pub site_name: Option<String>,
    /// Non-secret login identifier (e-mail or username).
    pub login_id: Option<String>,
    /// Owner-only note (never sent to models).
    pub note: Option<String>,
    /// Exact origins the secret may be typed on.
    pub origins: Vec<String>,
    /// Verified desktop program identities the secret may be typed into.
    pub programs: Vec<String>,
    /// Bot ids allowed to use this secret. Empty = NO bot may use it (fail-closed).
    pub bot_ids: Vec<String>,
    /// Whether the bot must ask the owner before typing this secret.
    pub ask_before_use: bool,
    /// Whether the store holds a value for it.
    pub has_value: bool,
    /// Last change, ms since the epoch.
    pub updated_at_ms: u64,
}

/// Account facts of a secret a bot registered. Absent on the owner's own
/// secrets, which are scoped by `Entry::bot_ids` (none until the owner grants).
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
    /// Owner/bot-chosen tag that tells several accounts on one site apart.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    created_at_ms: u64,
    /// When the password was first typed anywhere (`None` = never: a sign-up
    /// that failed before typing leaves the account in this state).
    #[serde(default)]
    first_used_at_ms: Option<u64>,
}

/// Fields supplied when creating or updating a Vault / fill secret.
#[derive(Debug, Default, Clone, Copy)]
pub struct FillSecretUpdate<'a> {
    /// Secret value (required for new entries, optional on update).
    pub value: Option<&'a str>,
    /// Replaces allowed origins when `Some`.
    pub origins: Option<&'a [String]>,
    /// Replaces allowed desktop programs when `Some`.
    pub programs: Option<&'a [String]>,
    /// Replaces allowed bot ids when `Some`. Defaults to empty (`[]`) on create.
    pub bot_ids: Option<&'a [String]>,
    /// Replaces item kind when `Some`.
    pub kind: Option<VaultKind>,
    /// Replaces site name when `Some` (empty string clears it).
    pub site_name: Option<&'a str>,
    /// Replaces login identifier when `Some` (empty string clears it).
    pub login_id: Option<&'a str>,
    /// Replaces note when `Some` (empty string clears it).
    pub note: Option<&'a str>,
    /// Replaces `ask_before_use` when `Some` (defaults to `true` on create).
    pub ask_before_use: Option<bool>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<AccountMeta>,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    programs: Vec<String>,
    /// Per-bot scope: empty means NO bot may use this secret. Entries from
    /// before per-bot scope (version 1) start empty (fail-closed): the owner
    /// grants bot access in the Vault.
    #[serde(default)]
    bot_ids: Vec<String>,
    #[serde(default)]
    kind: Option<VaultKind>,
    #[serde(default)]
    site_name: Option<String>,
    #[serde(default)]
    login_id: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// Owner-confirmed reason for a credential supplied through a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_purpose: Option<String>,
    #[serde(default = "default_true")]
    ask_before_use: bool,
    #[serde(default)]
    updated_at_ms: u64,
    /// A two-factor key is stored for this (bot-made) account, under
    /// [`totp_key`]. The key itself never leaves the store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    totp: bool,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            account: None,
            origins: Vec::new(),
            programs: Vec::new(),
            bot_ids: Vec::new(),
            kind: None,
            site_name: None,
            login_id: None,
            note: None,
            request_purpose: None,
            ask_before_use: true,
            updated_at_ms: 0,
            totp: false,
        }
    }
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
    /// Tag that tells several accounts on one site apart (`None`: the site's first one).
    pub label: Option<String>,
    /// Whether the store holds the password.
    pub has_value: bool,
    /// Whether a two-factor key is stored too (`totp:<name>` types its code).
    pub has_totp: bool,
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
    /// Tag for a further account on the same site (`None`: the first one).
    pub label: Option<&'a str>,
    /// Password length (clamped to [`PASSWORD_MIN_LEN`]..=[`PASSWORD_MAX_LEN`]).
    pub password_len: usize,
    /// Allow symbols in the password (some sites refuse them).
    pub symbols: bool,
}

/// A value read from a page by the daemon (an API key or token shown after a
/// sign-up), for [`FillSecrets::capture_secret`].
#[derive(Debug, Clone)]
pub struct NewCapture<'a> {
    /// Secret name to store under.
    pub name: &'a str,
    /// Origin of the page the value came from: the only place it may be typed.
    pub site: &'a str,
    /// What the value is, for the owner and the bot (`api key`).
    pub what: &'a str,
    /// The bot that captured it and the only one that may use it.
    pub bot_id: &'a str,
    /// Short note.
    pub note: Option<&'a str>,
    /// Tag that ties it to one of several accounts on the site.
    pub label: Option<&'a str>,
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

fn clean_bot_id(raw: &str) -> Result<String, FillError> {
    let t = raw.trim();
    if t.is_empty()
        || t.len() > MAX_BOT_ID_BYTES
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(FillError::InvalidEntry(format!(
            "`{}`: a bot id is 1 to 64 ASCII letters, digits, '-' or '_'",
            sanitize_label(raw, 40)
        )));
    }
    Ok(t.to_owned())
}

fn clean_bot_ids(list: &[String]) -> Result<Vec<String>, FillError> {
    if list.len() > MAX_BOTS {
        return Err(FillError::Invalid("at most 64 bots per secret".into()));
    }
    let set: BTreeSet<String> = list
        .iter()
        .map(|b| clean_bot_id(b))
        .collect::<Result<_, _>>()?;
    Ok(set.into_iter().collect())
}

fn clean_optional_label(raw: &str, cap: usize) -> Option<String> {
    let cleaned = sanitize_label(raw, cap);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn clean_optional_note(raw: &str, cap: usize) -> Option<String> {
    let mut out = String::new();
    let mut prev_nl = false;
    for c in raw.chars() {
        if c == '\r' {
            continue;
        }
        if c == '\n' {
            if !out.is_empty() && !prev_nl {
                out.push('\n');
                prev_nl = true;
            }
            continue;
        }
        let hidden = c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
        if hidden {
            continue;
        }
        prev_nl = false;
        out.push(c);
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > cap {
        let mut cut: String = trimmed.chars().take(cap.saturating_sub(1)).collect();
        cut.push('…');
        Some(cut)
    } else {
        Some(trimmed.to_owned())
    }
}

fn effective_kind(name: &str, e: &Entry) -> VaultKind {
    if let Some(kind) = e.kind {
        return kind;
    }
    if !e.origins.is_empty() || e.login_id.is_some() || name.starts_with("login/") {
        VaultKind::Login
    } else {
        VaultKind::Other
    }
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

/// Who a multi-entry write is for.
#[derive(Clone, Copy)]
enum WriteScope<'a> {
    /// The owner's own write: no bot scope applies.
    Owner,
    /// An existing vault write on behalf of a bot, with optional non-secret metadata.
    Bot {
        requested_by: Option<&'a str>,
        login_id: Option<&'a str>,
    },
    /// The owner answered a browser-only secret request for this bot.
    Request { bot: &'a str, purpose: &'a str },
}

fn store_key(name: &str) -> String {
    format!("{FILL_PREFIX}{name}")
}

/// Where the two-factor key of account `name` is kept. The first segment starts
/// with `__`, which [`validate_fill_name`] refuses, so no name a model or the
/// owner can pick ever maps onto it.
fn totp_key(name: &str) -> String {
    format!("{FILL_PREFIX}__totp/{name}")
}

/// What a `secret` reference starts with to ask for the current two-factor
/// code of an account instead of its password: `totp:<name>`. A `:` is not
/// allowed in a fill name, so the form can never be a stored secret's name.
pub const TOTP_REF_PREFIX: &str = "totp:";

/// The account name inside a `totp:<name>` reference, or `None` for an
/// ordinary secret name.
#[must_use]
pub fn totp_reference(secret: &str) -> Option<&str> {
    secret
        .get(..TOTP_REF_PREFIX.len())
        .filter(|p| p.eq_ignore_ascii_case(TOTP_REF_PREFIX))
        .map(|_| &secret[TOTP_REF_PREFIX.len()..])
}

/// Refuse a `totp:<name>` reference where no web page is involved (a desktop
/// program, the owner's reveal, a standing grant, the plain read path): a code
/// is made only by [`FillSecrets::resolve_totp_for_page`], for a field of the
/// account's own site. Explicit, so none of these paths relies on the `:` in
/// the reference failing name validation.
fn refuse_totp_reference(name: &str) -> Result<(), FillError> {
    if totp_reference(name).is_some() {
        return Err(FillError::NotAllowed(
            "a two-factor code is typed only into a field of the account's own web page; it is never given to a desktop program, shown, granted for good or read here".into(),
        ));
    }
    Ok(())
}

fn unix_secs() -> u64 {
    now_ms() / 1000
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
        let mut meta = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Meta>(&bytes)
                .map_err(|_| unavailable("the metadata file is not valid JSON"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Meta::default(),
            Err(_) => return Err(unavailable("the metadata file cannot be read")),
        };
        if meta.version > META_VERSION {
            return Err(unavailable("the metadata file is from a newer version"));
        }
        // Migrating v1 -> v2 (fail-closed per-bot scope): the owner's secrets
        // keep `bot_ids: []`, so no bot may use them until the owner picks the
        // bots in the Vault (nothing recorded which bot used a secret). A secret
        // that already had standing grants keeps `ask_before_use = false`, so the
        // owner's "do not ask again" choice survives once a bot is allowed.
        if meta.version < META_VERSION {
            let granted: BTreeSet<String> = meta.grants.iter().map(|g| g.secret.clone()).collect();
            for (name, entry) in &mut meta.secrets {
                if granted.contains(name) {
                    entry.ask_before_use = false;
                }
            }
            meta.version = META_VERSION;
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
            kind: if e.account.is_some() {
                e.kind.unwrap_or(VaultKind::Login)
            } else {
                effective_kind(name, e)
            },
            site_name: e.site_name.clone(),
            login_id: e
                .login_id
                .clone()
                .or_else(|| e.account.as_ref().map(|a| a.login_id.clone())),
            note: e
                .note
                .clone()
                .or_else(|| e.account.as_ref().and_then(|a| a.note.clone())),
            origins: e.origins.clone(),
            programs: e.programs.clone(),
            bot_ids: Self::scoped_bots(e),
            ask_before_use: e.ask_before_use,
            has_value: present.contains(&store_key(name)),
            updated_at_ms: e.updated_at_ms,
        }
    }

    fn sync_grants_for_entry(meta: &mut Meta, name: &str) -> Result<(), FillError> {
        let Some(entry) = meta.secrets.get(name).cloned() else {
            return Ok(());
        };
        if entry.ask_before_use {
            meta.grants.retain(|g| g.secret != name);
            return Ok(());
        }
        // A grant is only valid while its origin stays on the allow-list.
        let allowed: BTreeSet<String> = entry.origins.iter().cloned().collect();
        meta.grants
            .retain(|g| g.secret != name || allowed.contains(&g.origin));
        for origin in &entry.origins {
            let exists = meta.grants.iter().any(|g| {
                g.secret == name && g.origin == *origin && g.kind == GRANT_KIND_BROWSER_FILL
            });
            if !exists {
                if meta.grants.len() >= MAX_GRANTS {
                    return Err(FillError::Invalid("too many standing grants".into()));
                }
                meta.grants.push(FillGrant {
                    id: uuid::Uuid::now_v7().to_string(),
                    secret: name.to_owned(),
                    origin: origin.clone(),
                    kind: GRANT_KIND_BROWSER_FILL.to_owned(),
                    created_at_ms: now_ms(),
                });
            }
        }
        Ok(())
    }

    /// Create or update a secret with optional Vault metadata and bot access.
    /// `value` is write-only: required when the secret is new, optional (keep
    /// the stored value) on update. `bot_ids` defaults to empty (`[]`, no bot)
    /// on creation.
    ///
    /// # Errors
    /// Invalid name / entries / value, a limit, or a backend failure.
    pub fn upsert_with(
        &self,
        name: &str,
        update: FillSecretUpdate<'_>,
    ) -> Result<FillSecretInfo, FillError> {
        validate_fill_name(name)?;
        if let Some(v) = update.value {
            check_value_size(v)?;
        }
        let origins = update.origins.map(clean_origins).transpose()?;
        let programs = update.programs.map(clean_programs).transpose()?;
        let bot_ids = update.bot_ids.map(clean_bot_ids).transpose()?;

        let mut meta = self.meta.lock();
        let existed = meta.secrets.contains_key(name);
        if !existed {
            if update.value.is_none() {
                return Err(FillError::Invalid("a new fill secret needs a value".into()));
            }
            if meta.secrets.len() >= MAX_SECRETS {
                return Err(FillError::Invalid("too many fill secrets".into()));
            }
        }
        let before = meta.clone();
        let previous_value = if update.value.is_some() && existed {
            self.store
                .get(&store_key(name))
                .map_err(|_| unavailable("the secret store cannot be read"))?
        } else {
            None
        };
        if let Some(v) = update.value {
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
        if let Some(b) = bot_ids {
            if let Some(a) = entry.account.as_mut() {
                a.bot_ids.clone_from(&b);
            }
            entry.bot_ids = b;
        }
        if let Some(k) = update.kind {
            entry.kind = Some(k);
        }
        if let Some(s) = update.site_name {
            entry.site_name = clean_optional_label(s, MAX_SITE_NAME_CHARS);
        }
        if let Some(l) = update.login_id {
            entry.login_id = clean_optional_label(l, MAX_LOGIN_ID_CHARS);
        }
        if let Some(n) = update.note {
            entry.note = clean_optional_note(n, MAX_NOTE_CHARS);
        }
        if let Some(ask) = update.ask_before_use {
            entry.ask_before_use = ask;
        }
        entry.updated_at_ms = now_ms();

        let sync_res = if update.ask_before_use.is_some() {
            Self::sync_grants_for_entry(&mut meta, name)
        } else {
            // Keep standing grants valid only for still-allowed origins.
            let allowed: BTreeSet<String> = entry.origins.iter().cloned().collect();
            meta.grants
                .retain(|g| g.secret != name || allowed.contains(&g.origin));
            Ok(())
        };
        if let Err(e) = sync_res {
            *meta = before;
            if let Some(prev) = previous_value {
                let _ = self.store.set(&store_key(name), prev.expose());
            } else if !existed && update.value.is_some() {
                let _ = self.store.delete(&store_key(name));
            }
            return Err(e);
        }
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            if let Some(prev) = previous_value {
                let _ = self.store.set(&store_key(name), prev.expose());
            } else if !existed && update.value.is_some() {
                let _ = self.store.delete(&store_key(name));
            }
            return Err(e);
        }
        let present = self.present_in_store()?;
        let entry = meta.secrets.get(name).cloned().unwrap_or_default();
        Ok(Self::info(name, &entry, &present))
    }

    /// Create or update a secret. `value` is write-only: required when the
    /// secret is new, optional (keep the stored value) on update. `origins` /
    /// `programs` replace the allow-lists when given. New secrets start with no
    /// bot access (`bot_ids = []`) until the owner grants it.
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
        self.upsert_with(
            name,
            FillSecretUpdate {
                value,
                origins,
                programs,
                ..FillSecretUpdate::default()
            },
        )
    }

    /// Write several entries as one unit (a login: username and password),
    /// with no bot access (see [`Self::upsert_all_for_bots`]).
    ///
    /// # Errors
    /// As [`Self::upsert_all_for_bots`].
    pub fn upsert_all(
        &self,
        items: &[(String, String)],
        origins: &[String],
        programs: &[String],
        replace: bool,
    ) -> Result<bool, FillError> {
        self.upsert_all_scoped(items, origins, programs, &[], replace, WriteScope::Owner)
    }

    /// [`Self::upsert_all`] for a write that a BOT asked for (a `secret_request` the
    /// owner answers): an existing entry that is another bot's account is refused
    /// ([`FillError::NotAllowed`]) before anything is written, under the same lock as
    /// the write, so bot B cannot get the owner to overwrite bot A's account value
    /// whatever the form said; the same holds for an owner-made or owner-granted entry
    /// the bot may not use. An existing entry the bot may use keeps its scope, account
    /// facts, origins and programs (only the value changes); new entries are scoped to
    /// `bot` (none when `bot` is `None`).
    ///
    /// # Errors
    /// As [`Self::upsert_all`], plus [`FillError::NotAllowed`] for an existing entry the bot
    /// may not use.
    pub fn upsert_all_for_bot(
        &self,
        items: &[(String, String)],
        origins: &[String],
        programs: &[String],
        replace: bool,
        bot: Option<&str>,
    ) -> Result<bool, FillError> {
        let bots: Vec<String> = bot.map(str::to_owned).into_iter().collect();
        self.upsert_all_for_bots(items, origins, programs, &bots, None, replace, bot)
    }

    /// [`Self::upsert_all_for_bot`] with an explicit bot allow-list and optional
    /// non-secret login display metadata. Use [`Self::store_request`] for private
    /// values submitted through a credential request.
    ///
    /// # Errors
    /// As [`Self::upsert_all_for_bot`].
    #[allow(clippy::too_many_arguments)] // Existing Vault write API.
    pub fn upsert_all_for_bots(
        &self,
        items: &[(String, String)],
        origins: &[String],
        programs: &[String],
        bot_ids: &[String],
        login_id: Option<&str>,
        replace: bool,
        requested_by: Option<&str>,
    ) -> Result<bool, FillError> {
        self.upsert_all_scoped(
            items,
            origins,
            programs,
            bot_ids,
            replace,
            WriteScope::Bot {
                requested_by,
                login_id,
            },
        )
    }

    /// Store an owner-confirmed request for exactly one bot and HTTPS origin.
    /// Both fields of a login are secret values. The purpose is vault metadata.
    /// Replacing an entry requires the same exclusive bot and origin, and clears
    /// any standing grants so the new credential starts with ask-before-use.
    ///
    /// # Errors
    /// As [`Self::upsert_all_for_bot`].
    pub fn store_request(
        &self,
        items: &[(String, SecretValue)],
        origin: &str,
        bot: &str,
        purpose: &str,
        replace: bool,
    ) -> Result<bool, FillError> {
        if items
            .iter()
            .any(|(_, value)| value.expose().len() < crate::MIN_REDACTABLE_SECRET_BYTES)
        {
            return Err(FillError::InvalidEntry(format!(
                "each requested credential field must be at least {} bytes so it can be redacted",
                crate::MIN_REDACTABLE_SECRET_BYTES,
            )));
        }
        let origin = canonical_origin(origin)?;
        if !origin.starts_with("https://") {
            return Err(FillError::InvalidEntry(
                "a request requires an HTTPS origin".into(),
            ));
        }
        let bot = clean_bot_id(bot)?;
        let purpose = sanitize_label(purpose, 200);
        if purpose.is_empty() {
            return Err(FillError::InvalidEntry(
                "a request requires a purpose".into(),
            ));
        }
        let items: Vec<_> = items.iter().map(|(n, v)| (n.clone(), v.expose())).collect();
        self.upsert_all_scoped(
            &items,
            &[origin],
            &[],
            std::slice::from_ref(&bot),
            replace,
            WriteScope::Request {
                bot: &bot,
                purpose: &purpose,
            },
        )
    }

    /// Write several entries as one unit (a login: username and password):
    /// everything is validated first, and when any write or the metadata save
    /// fails the previous values and metadata are put back, so the vault never
    /// holds a mix of old and new. Returns whether an existing entry was
    /// replaced. `scope`: who the write is for (see [`WriteScope`]).
    #[allow(clippy::too_many_lines)] // One locked write, with rollback on failure.
    fn upsert_all_scoped<V: AsRef<str>>(
        &self,
        items: &[(String, V)],
        origins: &[String],
        programs: &[String],
        bot_ids: &[String],
        replace: bool,
        scope: WriteScope<'_>,
    ) -> Result<bool, FillError> {
        for (n, v) in items {
            validate_fill_name(n)?;
            check_value_size(v.as_ref())?;
        }
        let origins = clean_origins(origins)?;
        let programs = clean_programs(programs)?;
        let bot_ids = clean_bot_ids(bot_ids)?;
        let mut meta = self.meta.lock();
        for (n, _) in items {
            let Some(entry) = meta.secrets.get(n) else {
                continue;
            };
            match scope {
                WriteScope::Owner => {}
                WriteScope::Bot { requested_by, .. } => Self::scope_check(n, entry, requested_by)?,
                WriteScope::Request { bot, .. } => {
                    Self::scope_check(n, entry, Some(bot))?;
                    if replace
                        && (entry.origins != origins
                            || !entry.programs.is_empty()
                            || entry.bot_ids != bot_ids
                            || entry.account.is_some())
                    {
                        return Err(FillError::NotAllowed(
                            "a request can only replace an entry restricted to the same bot and origin".into(),
                        ));
                    }
                }
            }
        }
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
            if self.store.set(&store_key(n), v.as_ref()).is_err() {
                restore(i + 1);
                return Err(unavailable("the secret store rejected the write"));
            }
            self.registered(v.as_ref());
        }
        let before = meta.clone();
        for (n, _) in items {
            // A bot-scoped write over an entry that already exists (the bot's own, as
            // checked above) changes the VALUE only: the scope, account facts, origins
            // and programs stay as the owner set them. A form cannot widen them.
            let keep_scope =
                matches!(scope, WriteScope::Bot { .. }) && meta.secrets.contains_key(n);
            let entry = meta.secrets.entry(n.clone()).or_default();
            if !keep_scope {
                entry.origins.clone_from(&origins);
                entry.programs.clone_from(&programs);
                entry.bot_ids.clone_from(&bot_ids);
                if let Some(a) = entry.account.as_mut() {
                    a.bot_ids.clone_from(&bot_ids);
                }
            }
            let request = if let WriteScope::Request { purpose, .. } = scope {
                entry.request_purpose = Some(purpose.to_owned());
                entry.ask_before_use = true;
                // Older requests copied the submitted username here.
                entry.login_id = None;
                true
            } else {
                if let WriteScope::Bot {
                    login_id: Some(login_id),
                    ..
                } = scope
                    && let Some(label) = clean_optional_label(login_id, MAX_LOGIN_ID_CHARS)
                {
                    entry.login_id = Some(label);
                }
                false
            };
            entry.updated_at_ms = now_ms();
            let allowed: BTreeSet<String> = entry.origins.iter().cloned().collect();
            meta.grants
                .retain(|g| g.secret != *n || (!request && allowed.contains(&g.origin)));
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
        Self::check_bot_quota(&meta, new.bot_id)?;
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
                label: new
                    .label
                    .map(|l| sanitize_label(l, 40))
                    .filter(|l| !l.is_empty()),
                created_at_ms: now,
                first_used_at_ms: None,
            }),
            origins: vec![site],
            programs: Vec::new(),
            bot_ids: vec![new.bot_id.to_owned()],
            updated_at_ms: now,
            ..Entry::default()
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

    /// Keep a value the daemon read from a page (see `browser_capture_secret`): stored
    /// under `new.name`, limited to the origin it was read on and to `new.bot_id`, listed
    /// with the bot's accounts as a `token`. The value goes only into the store and the
    /// redaction hook; it is not returned. Capturing again under the name of the bot's own
    /// earlier capture replaces the value (a rotated key); any other existing name is refused.
    ///
    /// # Errors
    /// Invalid name / site / value, the name belongs to something else
    /// ([`FillError::Exists`]), a limit, or a backend failure. Nothing is left behind on failure.
    pub fn capture_secret(
        &self,
        new: &NewCapture<'_>,
        value: &SecretValue,
    ) -> Result<AccountInfo, FillError> {
        validate_fill_name(new.name)?;
        let site = canonical_origin(new.site)?;
        let what = sanitize_label(new.what, 60);
        if what.is_empty() {
            return Err(FillError::Invalid("say what the value is".into()));
        }
        if new.bot_id.is_empty() {
            return Err(FillError::Invalid(
                "a captured value belongs to a bot; there is no bot in this call".into(),
            ));
        }
        let v = value.expose();
        if v.is_empty()
            || v.len() > MAX_CAPTURED_BYTES
            || v.chars().any(|c| c.is_control() && c != '\n')
        {
            return Err(FillError::Invalid(
                "the element holds no usable value (empty, over 4 KiB or with control characters)"
                    .into(),
            ));
        }
        let mut meta = self.meta.lock();
        let replacing = match meta.secrets.get(new.name) {
            None => false,
            Some(e)
                if Self::bot_may_use(e, Some(new.bot_id))
                    && e.account
                        .as_ref()
                        .is_some_and(|a| a.created_by == "bot" && a.kind == "token") =>
            {
                true
            }
            Some(_) => return Err(FillError::Exists(new.name.to_owned())),
        };
        if !replacing && meta.secrets.len() >= MAX_SECRETS {
            return Err(FillError::Invalid("too many fill secrets".into()));
        }
        if !replacing {
            Self::check_bot_quota(&meta, new.bot_id)?;
        }
        let before = meta.clone();
        let previous = self.store.get(&store_key(new.name)).ok().flatten();
        self.store
            .set(&store_key(new.name), v)
            .map_err(|_| unavailable("the secret store rejected the write"))?;
        self.registered(v);
        let now = now_ms();
        let entry = Entry {
            account: Some(AccountMeta {
                site: site.clone(),
                login_id: what,
                kind: "token".into(),
                bot_ids: vec![new.bot_id.to_owned()],
                created_by: "bot".into(),
                email_account: None,
                note: new.note.map(|n| sanitize_label(n, 200)),
                label: new
                    .label
                    .map(|l| sanitize_label(l, 40))
                    .filter(|l| !l.is_empty()),
                created_at_ms: now,
                first_used_at_ms: None,
            }),
            origins: vec![site],
            programs: Vec::new(),
            bot_ids: vec![new.bot_id.to_owned()],
            kind: Some(VaultKind::Other),
            updated_at_ms: now,
            ..Entry::default()
        };
        meta.secrets.insert(new.name.to_owned(), entry.clone());
        // A rotated value starts without the old value's standing grants.
        meta.grants.retain(|g| g.secret != new.name);
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            let _ = match previous {
                Some(p) => self.store.set(&store_key(new.name), p.expose()),
                None => self.store.delete(&store_key(new.name)).map(|_| ()),
            };
            return Err(e);
        }
        let present = self.present_in_store()?;
        Self::account_info(new.name, &entry, &present)
            .ok_or_else(|| unavailable("the value was not recorded"))
    }

    /// How many accounts and captured values `bot` made for itself.
    fn bot_own_count(meta: &Meta, bot: &str) -> usize {
        meta.secrets
            .values()
            .filter(|e| {
                e.account
                    .as_ref()
                    .is_some_and(|a| a.created_by == "bot" && a.bot_ids.iter().any(|b| b == bot))
            })
            .count()
    }

    fn check_bot_quota(meta: &Meta, bot: &str) -> Result<(), FillError> {
        if Self::bot_own_count(meta, bot) >= MAX_BOT_OWN_SECRETS {
            return Err(FillError::Invalid(format!(
                "this bot already holds {MAX_BOT_OWN_SECRETS} accounts and captured values of its own; account_discard removes ones it no longer needs"
            )));
        }
        Ok(())
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
            label: a.label.clone(),
            has_value: present.contains(&store_key(name)),
            has_totp: e.totp && present.contains(&totp_key(name)),
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

    /// Store the two-factor setup secret (an `otpauth://totp/` address or a
    /// base32 key) of an account `bot` registered itself, next to its password.
    /// The key is bound to that account's bot and origin: it is typed only
    /// through `totp:<name>` fills, into a field on the account's site. It is
    /// never returned, listed or revealed. Saving again replaces the key.
    ///
    /// The owner's own secrets are refused: their two-factor codes stay theirs.
    ///
    /// # Errors
    /// Unknown name, not this bot's bot-made account, a setup text that is not a
    /// usable TOTP secret ([`FillError::Invalid`], never echoing it), or a
    /// backend failure. Nothing is changed on failure.
    pub fn set_totp(&self, name: &str, bot: Option<&str>, setup: &str) -> Result<(), FillError> {
        validate_fill_name(name)?;
        // Strict first (a bare key or an address), then the text of a page element
        // (a labelled key): the caller read it host-side, it never came from the model.
        let spec = TotpSpec::parse(setup).or_else(|_| TotpSpec::find_in_text(setup))?;
        let mut meta = self.meta.lock();
        let entry = meta
            .secrets
            .get(name)
            .ok_or_else(|| FillError::Unknown(name.to_owned()))?;
        Self::scope_check(name, entry, bot)?;
        Self::require_bot_made(name, entry)?;
        let key = totp_key(name);
        let previous = self
            .store
            .get(&key)
            .map_err(|_| unavailable("the secret store cannot be read"))?;
        let canonical = spec.canonical();
        // The key goes to the redactor before it is stored: from here on no
        // output of any tool can carry it, in any spelling a page or a person
        // can give it (spaced, dashed, either case, padded or not, the address
        // as it was read).
        self.register_seed(setup, &spec);
        self.store
            .set(&key, &canonical)
            .map_err(|_| unavailable("the secret store rejected the write"))?;
        let before = meta.clone();
        if let Some(e) = meta.secrets.get_mut(name) {
            e.totp = true;
            e.updated_at_ms = now_ms();
        }
        if let Err(e) = self.persist(&meta) {
            *meta = before;
            match previous {
                Some(old) => {
                    let _ = self.store.set(&key, old.expose());
                }
                None => {
                    let _ = self.store.delete(&key);
                }
            }
            return Err(e);
        }
        Ok(())
    }

    /// Teach the redactor every spelling of a setup key, and the text it was read from.
    fn register_seed(&self, raw: &str, spec: &TotpSpec) {
        self.registered(raw);
        self.registered(raw.trim());
        for spelling in spec.seed_spellings() {
            self.registered(&spelling);
        }
    }

    /// Whether the setup key of account `name` may be read off the page whose frame
    /// and top origin are given (as the browser reports them): the account is this
    /// bot's own, bot-made, and BOTH origins are the account's own site. Checked
    /// before the element is read and again at the read.
    ///
    /// # Errors
    /// Unknown name, another bot's or an owner's secret, or an origin that is not
    /// the account's own.
    pub fn check_totp_save_page(
        &self,
        name: &str,
        bot: Option<&str>,
        frame_origin: Option<&str>,
        top_origin: Option<&str>,
    ) -> Result<(), FillError> {
        validate_fill_name(name)?;
        let meta = self.meta.lock();
        let e = meta
            .secrets
            .get(name)
            .ok_or_else(|| FillError::Unknown(name.to_owned()))?;
        Self::scope_check(name, e, bot)?;
        Self::require_bot_made(name, e)?;
        Self::check_origin(name, e, frame_origin)?;
        Self::check_origin(name, e, top_origin)?;
        Ok(())
    }

    /// Only an account a bot made itself may carry a two-factor key.
    fn require_bot_made(name: &str, e: &Entry) -> Result<(), FillError> {
        if e.account.as_ref().is_some_and(|a| a.created_by == "bot") {
            Ok(())
        } else {
            Err(FillError::NotAllowed(format!(
                "the secret `{}` is not an account this bot registered; two-factor codes are only kept for those",
                sanitize_label(name, 80)
            )))
        }
    }

    /// The checks every use of `totp:<name>` passes, on top of the bot scope.
    fn totp_ready(name: &str, e: &Entry) -> Result<(), FillError> {
        Self::require_bot_made(name, e)?;
        if e.totp {
            Ok(())
        } else {
            Err(FillError::NotAllowed(format!(
                "no two-factor key is stored for `{}`; save the setup secret with account_totp_save first",
                sanitize_label(name, 80)
            )))
        }
    }

    /// Whether `origin` (as the browser reported it) is on the entry's allow-list.
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] for no verifiable origin or one that is not listed.
    fn check_origin(shown: &str, e: &Entry, origin: Option<&str>) -> Result<String, FillError> {
        let origin = origin
            .and_then(|o| canonical_origin(o).ok())
            .ok_or_else(|| {
                FillError::NotAllowed(format!(
                    "refused: the page frame that holds this field has no verifiable web origin, so `{shown}` is not typed there"
                ))
            })?;
        if e.origins.contains(&origin) {
            Ok(origin)
        } else {
            Err(FillError::NotAllowed(format!(
                "refused: `{shown}` is not allowed on {origin}; only the owner can add an origin to it"
            )))
        }
    }

    /// The current two-factor code of account `name` for a field of a web page:
    /// `frame_origin` is the origin of the frame that holds the field and
    /// `top_origin` that of the page, as the browser reports them. The origin
    /// binding is enforced HERE, in the read itself, together with the bot
    /// scope and the bot-made flag; [`Self::check_browser_in`] stays as the
    /// pre-check that refuses before a card is shown.
    ///
    /// # Errors
    /// Unknown name, another bot's or an owner's secret, no key saved, an origin
    /// that is not the account's own (either of the two), or a backend failure.
    pub fn resolve_totp_for_page(
        &self,
        name: &str,
        bot: Option<&str>,
        frame_origin: Option<&str>,
        top_origin: Option<&str>,
    ) -> Result<SecretValue, FillError> {
        let base = totp_reference(name).unwrap_or(name);
        self.resolve_totp(base, bot, unix_secs(), frame_origin, top_origin)
    }

    /// The current two-factor code of account `name` at `now` (seconds since
    /// the epoch). The only read path to the key; the key never leaves here.
    ///
    /// The page the code is for is part of the call: `frame` is the origin of
    /// the frame that holds the field, `top` the origin of its page, both as
    /// the browser reports them. Both must be the account's own origin (see
    /// [`Self::check_origin`]); no caller can get a code for a page this does
    /// not accept, whatever it checked before.
    fn resolve_totp(
        &self,
        name: &str,
        bot: Option<&str>,
        now: u64,
        frame: Option<&str>,
        top: Option<&str>,
    ) -> Result<SecretValue, FillError> {
        validate_fill_name(name)?;
        match self.meta.lock().secrets.get(name) {
            None => return Err(FillError::Unknown(name.to_owned())),
            Some(e) => {
                Self::scope_check(name, e, bot)?;
                Self::totp_ready(name, e)?;
                Self::check_origin(&format!("{TOTP_REF_PREFIX}{name}"), e, frame)?;
                Self::check_origin(&format!("{TOTP_REF_PREFIX}{name}"), e, top)?;
            }
        }
        let stored = self
            .store
            .get(&totp_key(name))
            .map_err(|_| unavailable("the secret store cannot be read"))?
            .ok_or_else(|| FillError::MissingValue(format!("totp:{name}")))?;
        let spec = TotpSpec::parse(stored.expose())?;
        let code = spec.code_at(now);
        // The code is short-lived but a later page read could echo it back from
        // the field; mask it like any other typed secret.
        self.registered(&code);
        Ok(SecretValue::new(code))
    }

    /// Delete an account its own bot made, with its password and grants (a
    /// failed sign-up must not block the retry for ever). The owner's own
    /// secrets and other bots' accounts are refused.
    ///
    /// # Errors
    /// Unknown name, not this bot's bot-made account, or a backend failure.
    pub fn discard_account(&self, name: &str, bot: Option<&str>) -> Result<(), FillError> {
        validate_fill_name(name)?;
        // ONE lock for the check and the delete: what was checked is what is deleted.
        let mut meta = self.meta.lock();
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
        #[cfg(test)]
        tests::between_check_and_delete(self);
        self.delete_locked(&mut meta, name).map(|_| ())
    }

    /// The bots that may use an entry (the owner's pick in the Vault, plus the
    /// bot that registered an account).
    fn scoped_bots(e: &Entry) -> Vec<String> {
        let mut bots = e.bot_ids.clone();
        if let Some(a) = &e.account {
            for b in &a.bot_ids {
                if !bots.contains(b) {
                    bots.push(b.clone());
                }
            }
        }
        bots.sort();
        bots
    }

    /// Whether `bot` may use the secret: only a bot the owner (or, for its own
    /// account, the bot itself) put on the entry's list. No bot id, or an empty
    /// list, means no. This is the single gate every read and check goes through.
    fn bot_may_use(e: &Entry, bot: Option<&str>) -> bool {
        let Some(bot) = bot.map(str::trim).filter(|b| !b.is_empty()) else {
            return false;
        };
        e.bot_ids.iter().any(|x| x == bot)
            || e.account
                .as_ref()
                .is_some_and(|a| a.bot_ids.iter().any(|x| x == bot))
    }

    /// Refuse use of a bot-scoped account by a bot that does not own it.
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] (names the secret, nothing else).
    pub fn check_bot(&self, name: &str, bot: Option<&str>) -> Result<(), FillError> {
        let meta = self.meta.lock();
        if let Some(base) = totp_reference(name) {
            // A code is only ever made for an existing account, never "free".
            let e = meta
                .secrets
                .get(base)
                .ok_or_else(|| FillError::Unknown(base.to_owned()))?;
            Self::scope_check(base, e, bot)?;
            return Self::totp_ready(base, e);
        }
        match meta.secrets.get(name) {
            Some(e) => Self::scope_check(name, e, bot),
            None => Ok(()),
        }
    }

    /// Whether a `secret_request` of `bot` may write over `name`: an unknown name is
    /// free, an existing entry (account or owner-made) only when the bot may use it.
    /// Used where the OWNER sees the outcome (the submit route); a bot must not learn
    /// from it whether a name it cannot use exists (see [`Self::exists_for_bot`]).
    ///
    /// # Errors
    /// [`FillError::NotAllowed`] (names the secret, nothing else).
    pub fn check_request_target(&self, name: &str, bot: Option<&str>) -> Result<(), FillError> {
        let meta = self.meta.lock();
        match meta.secrets.get(name) {
            Some(e) => Self::scope_check(name, e, bot),
            None => Ok(()),
        }
    }

    /// Whether `name` exists AND `bot` may use it: the only existence a bot may
    /// learn. An entry the bot may not use answers exactly like a free name.
    #[must_use]
    pub fn exists_for_bot(&self, name: &str, bot: Option<&str>) -> bool {
        self.meta
            .lock()
            .secrets
            .get(name)
            .is_some_and(|e| Self::bot_may_use(e, bot))
    }

    fn scope_check(name: &str, e: &Entry, bot: Option<&str>) -> Result<(), FillError> {
        if Self::bot_may_use(e, bot) {
            Ok(())
        } else {
            Err(FillError::NotAllowed(format!(
                "the secret `{}` is not enabled for this bot; only the owner can allow it in the Vault",
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
        self.delete_locked(&mut meta, name)
    }

    /// [`Self::delete`] for a caller that already holds the metadata lock.
    fn delete_locked(&self, meta: &mut Meta, name: &str) -> Result<bool, FillError> {
        let before = meta.clone();
        let existed = meta.secrets.remove(name).is_some();
        meta.grants.retain(|g| g.secret != name);
        if existed && let Err(e) = self.persist(meta) {
            *meta = before;
            return Err(e);
        }
        self.store
            .delete(&store_key(name))
            .map_err(|_| unavailable("the secret store rejected the delete"))?;
        self.store
            .delete(&totp_key(name))
            .map_err(|_| unavailable("the secret store rejected the delete"))?;
        Ok(existed)
    }

    /// Create a standing grant "secret `secret` on `origin` without asking".
    /// The origin must already be on the secret's allow-list.
    ///
    /// # Errors
    /// Unknown secret, origin not on its allow-list, a limit, or a write failure.
    pub fn grant_create(&self, secret: &str, origin: &str) -> Result<FillGrant, FillError> {
        refuse_totp_reference(secret)?;
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
        if let Some(e) = meta.secrets.get_mut(secret) {
            e.ask_before_use = false;
        }
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
        let revoked_secret = meta
            .grants
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.secret.clone());
        let n = meta.grants.len();
        meta.grants.retain(|g| g.id != id);
        let removed = meta.grants.len() != n;
        if let Some(secret) = revoked_secret
            && !meta.grants.iter().any(|g| g.secret == secret)
            && let Some(e) = meta.secrets.get_mut(&secret)
        {
            e.ask_before_use = true;
        }
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
    /// standing grant that matches this exact secret, origin and kind covers it
    /// (only when `ask_before_use` is off).
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
        // `totp:<name>` is checked against the account's own origin list, bot
        // scope and bot-made flag; it never has a standing grant (every code is
        // confirmed, like every password fill).
        let (base, is_totp) = totp_reference(name).map_or((name, false), |b| (b, true));
        validate_fill_name(base)?;
        let meta = self.meta.lock();
        let entry = meta
            .secrets
            .get(base)
            .ok_or_else(|| FillError::Unknown(base.to_owned()))?;
        Self::scope_check(base, entry, bot)?;
        if is_totp {
            Self::totp_ready(base, entry)?;
        }
        let origin = Self::check_origin(name, entry, live_origin)?;
        if is_totp || entry.ask_before_use {
            return Ok(None);
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
        refuse_totp_reference(name)?;
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
    /// read or type a secret the owner has not allowed that bot to use.
    ///
    /// # Errors
    /// Unknown name, another bot's account, no stored value, or a backend failure.
    pub fn resolve_value_for(
        &self,
        name: &str,
        bot: Option<&str>,
    ) -> Result<SecretValue, FillError> {
        // A code is made only for a field of a web page, with the page's origins
        // in the call: [`Self::resolve_totp_for_page`]. This path has none.
        refuse_totp_reference(name)?;
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

    /// Owner-only read path to a value (`POST /v1/fill-secrets/reveal` after
    /// owner confirmation and audit). Never call from a bot tool or replay.
    ///
    /// # Errors
    /// Unknown name, no stored value, or a backend failure.
    pub fn resolve_value_for_owner(&self, name: &str) -> Result<SecretValue, FillError> {
        refuse_totp_reference(name)?;
        validate_fill_name(name)?;
        if !self.meta.lock().secrets.contains_key(name) {
            return Err(FillError::Unknown(name.to_owned()));
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
            if let Some(v) = self
                .store
                .get(&totp_key(&n))
                .map_err(|_| unavailable("the secret store cannot be read"))?
                && let Ok(spec) = TotpSpec::parse(v.expose())
            {
                out.push(v.expose().to_owned());
                out.extend(spec.seed_spellings());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
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

    fn b(s: &str) -> Vec<String> {
        vec![s.to_owned()]
    }

    const BOT_A: &str = "bot-a";
    const BOT_B: &str = "bot-b";

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
            s.resolve_value_for_owner("login/github").unwrap().expose(),
            "hunter2-correct"
        );
        // Update keeps the value when none is given.
        s.upsert("login/github", None, Some(&o("https://gitlab.com")), None)
            .unwrap();
        assert_eq!(
            s.resolve_value_for_owner("login/github").unwrap().expose(),
            "hunter2-correct"
        );
    }

    #[test]
    fn a_name_outside_the_registry_never_reaches_other_store_entries() {
        let (store, s) = svc();
        store.set("composio/gmail", "composio-token-123").unwrap();
        for name in ["composio/gmail", "fill/../composio/gmail", "x"] {
            assert!(s.resolve_value_for_owner(name).is_err(), "{name}");
            assert!(s.resolve_value_for(name, Some(BOT_A)).is_err(), "{name}");
        }
        // Even a registered name only ever reads `fill/<name>`.
        s.upsert_with(
            "work/a",
            FillSecretUpdate {
                value: Some("a-value-123"),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.resolve_value_for("work/a", Some(BOT_A)).unwrap().expose(),
            "a-value-123"
        );
    }

    #[test]
    fn browser_check_is_exact_and_fails_closed() {
        let (_s, s) = svc();
        s.upsert_with(
            "login/github",
            FillSecretUpdate {
                value: Some("v-1234567"),
                origins: Some(&o("https://github.com")),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert!(
            s.check_browser("login/github", Some("https://github.com"), Some(BOT_A))
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
                    s.check_browser("login/github", live, Some(BOT_A)),
                    Err(FillError::NotAllowed(_))
                ),
                "{live:?}"
            );
        }
        assert!(matches!(
            s.check_browser("login/other", Some("https://github.com"), Some(BOT_A)),
            Err(FillError::Unknown(_))
        ));
    }

    #[test]
    fn per_bot_scope_defaults_empty_and_blocks_other_bots() {
        let (_s, s) = svc();
        // Default = no bot allowed (fail-closed).
        let created = s
            .upsert(
                "login/default",
                Some("v-default-1"),
                Some(&o("https://example.com")),
                None,
            )
            .unwrap();
        assert!(created.bot_ids.is_empty());
        assert!(created.ask_before_use);
        assert!(s.list_for_bot(Some(BOT_A)).unwrap().is_empty());
        assert!(s.list_for_bot(None).unwrap().is_empty());
        assert!(matches!(
            s.check_browser("login/default", Some("https://example.com"), Some(BOT_A)),
            Err(FillError::NotAllowed(_))
        ));
        assert!(matches!(
            s.check_browser("login/default", Some("https://example.com"), None),
            Err(FillError::NotAllowed(_))
        ));
        assert!(matches!(
            s.resolve_value_for("login/default", Some(BOT_A)),
            Err(FillError::NotAllowed(_))
        ));

        // Grant only BOT_A: BOT_A works, BOT_B is refused everywhere.
        s.upsert_with(
            "login/default",
            FillSecretUpdate {
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(s.list_for_bot(Some(BOT_A)).unwrap().len(), 1);
        assert!(s.list_for_bot(Some(BOT_B)).unwrap().is_empty());
        assert!(s.check_bot("login/default", Some(BOT_A)).is_ok());
        assert!(s.check_bot("login/default", Some(BOT_B)).is_err());
        assert!(s.check_bot("login/default", None).is_err());
        assert!(
            s.check_browser("login/default", Some("https://example.com"), Some(BOT_A))
                .is_ok()
        );
        assert!(matches!(
            s.check_browser("login/default", Some("https://example.com"), Some(BOT_B)),
            Err(FillError::NotAllowed(_))
        ));
        assert!(matches!(
            s.resolve_value_for("login/default", Some(BOT_B)),
            Err(FillError::NotAllowed(_))
        ));
    }

    #[test]
    fn v1_metadata_migrates_with_empty_bot_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        store.set("fill/login/legacy", "legacy-pass-123").unwrap();
        let v1_json = serde_json::json!({
            "version": 1,
            "secrets": {
                "login/legacy": {
                    "origins": ["https://legacy.test"],
                    "programs": [],
                    "updated_at_ms": 123
                }
            },
            "grants": [
                {
                    "id": "g1",
                    "secret": "login/legacy",
                    "origin": "https://legacy.test",
                    "kind": "browser_fill",
                    "created_at_ms": 123
                }
            ]
        });
        std::fs::write(
            metadata_path(dir.path()),
            serde_json::to_vec_pretty(&v1_json).unwrap(),
        )
        .unwrap();

        let s = FillSecrets::open(store, dir.path()).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(
            list[0].bot_ids.is_empty(),
            "migrated v1 secrets fail closed"
        );
        assert!(
            !list[0].ask_before_use,
            "existing v1 standing grant preserves ask_before_use=false"
        );
        assert_eq!(list[0].kind, VaultKind::Login);
        assert!(matches!(
            s.check_browser("login/legacy", Some("https://legacy.test"), Some(BOT_A)),
            Err(FillError::NotAllowed(_))
        ));
        // Owner grants BOT_A in Vault -> existing grant takes effect.
        s.upsert_with(
            "login/legacy",
            FillSecretUpdate {
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.check_browser("login/legacy", Some("https://legacy.test"), Some(BOT_A))
                .unwrap(),
            Some("g1".into())
        );
    }

    #[test]
    fn a_standing_grant_matches_only_its_secret_and_origin() {
        let (_s, s) = svc();
        s.upsert_with(
            "a/one",
            FillSecretUpdate {
                value: Some("value-one-1"),
                origins: Some(&["https://one.test".into(), "https://two.test".into()]),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        s.upsert_with(
            "a/two",
            FillSecretUpdate {
                value: Some("value-two-2"),
                origins: Some(&["https://one.test".into()]),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        // The origin must already be allowed.
        assert!(s.grant_create("a/one", "https://other.test").is_err());
        let g = s.grant_create("a/one", "https://one.test").unwrap();
        assert_eq!(
            s.check_browser("a/one", Some("https://one.test"), Some(BOT_A))
                .unwrap(),
            Some(g.id.clone())
        );
        assert_eq!(
            s.check_browser("a/one", Some("https://two.test"), Some(BOT_A))
                .unwrap(),
            None
        );
        assert_eq!(
            s.check_browser("a/two", Some("https://one.test"), Some(BOT_A))
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
            s.check_browser("a/one", Some("https://two.test"), Some(BOT_A))
                .unwrap(),
            None
        );
        // Toggling ask_before_use = false creates grants; toggling back true revokes them.
        s.upsert_with(
            "a/one",
            FillSecretUpdate {
                ask_before_use: Some(false),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert!(
            s.check_browser("a/one", Some("https://two.test"), Some(BOT_A))
                .unwrap()
                .is_some()
        );
        s.upsert_with(
            "a/one",
            FillSecretUpdate {
                ask_before_use: Some(true),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.check_browser("a/one", Some("https://two.test"), Some(BOT_A))
                .unwrap(),
            None
        );
        assert_eq!(s.grants().len(), 0);
        // Delete removes grants too.
        let g = s.grant_create("a/one", "https://two.test").unwrap();
        assert!(s.delete("a/one").unwrap());
        assert!(!s.grants().iter().any(|x| x.id == g.id));
    }

    #[test]
    fn desktop_needs_a_verified_identity_and_bot_scope() {
        let (_s, s) = svc();
        s.upsert_with(
            "app/vpn",
            FillSecretUpdate {
                value: Some("vpn-pass-123"),
                programs: Some(&["win:sha256:abc".into()]),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        s.check_desktop("app/vpn", Some("win:sha256:abc"), Some(BOT_A))
            .unwrap();
        assert!(
            s.check_desktop("app/vpn", Some("win:sha256:other"), Some(BOT_A))
                .is_err()
        );
        assert!(s.check_desktop("app/vpn", None, Some(BOT_A)).is_err());
        assert!(
            s.check_desktop("app/vpn", Some("win:sha256:abc"), Some(BOT_B))
                .is_err()
        );
        assert!(
            s.check_desktop("app/vpn", Some("win:sha256:abc"), None)
                .is_err()
        );
    }

    #[test]
    fn metadata_survives_a_restart_and_holds_no_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        {
            let s = FillSecrets::open(store.clone(), dir.path()).unwrap();
            s.upsert_with(
                "login/x",
                FillSecretUpdate {
                    value: Some("never-on-disk-123"),
                    origins: Some(&o("https://x.test")),
                    bot_ids: Some(&b(BOT_A)),
                    kind: Some(VaultKind::Login),
                    site_name: Some("Example"),
                    login_id: Some("alice@x.test"),
                    note: Some("work account"),
                    ask_before_use: Some(false),
                    ..FillSecretUpdate::default()
                },
            )
            .unwrap();
        }
        let raw = std::fs::read_to_string(metadata_path(dir.path())).unwrap();
        assert!(!raw.contains("never-on-disk-123"));
        assert!(raw.contains("login/x"));
        let s = FillSecrets::open(store, dir.path()).unwrap();
        let item = &s.list().unwrap()[0];
        assert_eq!(item.site_name.as_deref(), Some("Example"));
        assert_eq!(item.login_id.as_deref(), Some("alice@x.test"));
        assert_eq!(item.note.as_deref(), Some("work account"));
        assert_eq!(item.bot_ids, vec![BOT_A]);
        assert!(!item.ask_before_use);
        assert_eq!(s.grants().len(), 1);
        // A corrupt file fails closed.
        std::fs::write(metadata_path(dir.path()), b"{not json").unwrap();
        assert!(FillSecrets::open(Arc::new(MemoryStore::new()), dir.path()).is_err());
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
        s.upsert_with(
            "h/one",
            FillSecretUpdate {
                value: Some("hooked-value-1"),
                bot_ids: Some(&b(BOT_A)),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        s.resolve_value_for("h/one", Some(BOT_A)).unwrap();
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

    fn registered_account(s: &FillSecrets) -> &'static str {
        let name = "login/example.com-aaaa1111";
        s.register_account(&NewAccount {
            name,
            site: "https://example.com",
            login_id: "me",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            label: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
        name
    }

    fn capture<'a>(name: &'a str, site: &'a str, bot: &'a str) -> NewCapture<'a> {
        NewCapture {
            name,
            site,
            what: "api key",
            bot_id: bot,
            note: None,
            label: None,
        }
    }

    #[test]
    fn a_captured_value_is_bound_to_the_bot_and_the_origin_and_never_returned() {
        let (_, s) = svc();
        let name = "token/example.com-aaaa1111-key";
        let info = s
            .capture_secret(
                &capture(name, "https://example.com", "bot-a"),
                &SecretValue::new("sk-live-0123456789abcdef"),
            )
            .unwrap();
        assert_eq!(info.kind, "token");
        assert_eq!(info.login_id, "api key");
        assert_eq!(info.created_by, "bot");
        assert!(info.has_value && info.unused);
        assert!(!format!("{info:?}").contains("sk-live"));
        // Only the capturing bot, only on the origin the value was read on.
        let site = Some("https://example.com");
        assert!(s.check_browser(name, site, Some("bot-a")).is_ok());
        assert!(s.check_browser(name, site, Some("bot-b")).is_err());
        assert!(s.check_browser(name, site, None).is_err());
        assert!(
            s.check_browser(name, Some("https://evil.example"), Some("bot-a"))
                .is_err()
        );
        assert!(s.resolve_value_for(name, Some("bot-b")).is_err());
        assert_eq!(
            s.resolve_value_for(name, Some("bot-a")).unwrap().expose(),
            "sk-live-0123456789abcdef"
        );
        // It lists with the bot's accounts, not another bot's, and the bot may discard it.
        assert_eq!(s.accounts(Some("bot-a")).unwrap().len(), 1);
        assert!(s.accounts(Some("bot-b")).unwrap().is_empty());
        assert!(s.discard_account(name, Some("bot-b")).is_err());
        s.discard_account(name, Some("bot-a")).unwrap();
        assert!(!s.exists(name));
    }

    #[test]
    fn capturing_again_replaces_only_the_bots_own_capture() {
        let (store, s) = svc();
        let name = "token/example.com-aaaa1111-key";
        let cap = capture(name, "https://example.com", "bot-a");
        s.capture_secret(&cap, &SecretValue::new("first-value-123"))
            .unwrap();
        s.capture_secret(&cap, &SecretValue::new("rotated-value-456"))
            .unwrap();
        assert_eq!(
            s.resolve_value_for(name, Some("bot-a")).unwrap().expose(),
            "rotated-value-456"
        );
        // Another bot, an owner-made secret and a registered password are never overwritten.
        assert!(matches!(
            s.capture_secret(
                &capture(name, "https://example.com", "bot-b"),
                &SecretValue::new("x-value-789")
            ),
            Err(FillError::Exists(_))
        ));
        s.upsert("login/owner-made", Some("owner"), None, None)
            .unwrap();
        assert!(matches!(
            s.capture_secret(
                &capture("login/owner-made", "https://example.com", "bot-a"),
                &SecretValue::new("x-value-789")
            ),
            Err(FillError::Exists(_))
        ));
        let pw = registered_account(&s);
        assert!(matches!(
            s.capture_secret(
                &capture(pw, "https://example.com", "bot-a"),
                &SecretValue::new("x-value-789")
            ),
            Err(FillError::Exists(_))
        ));
        assert_ne!(
            store.get(&store_key(pw)).unwrap().unwrap().expose(),
            "x-value-789"
        );
    }

    #[test]
    fn one_bot_cannot_fill_the_store_with_its_own_entries() {
        let (_, s) = svc();
        let site = "https://example.com";
        for i in 0..MAX_BOT_OWN_SECRETS {
            let name = format!("token/example.com-aaaa1111-k{i}");
            s.capture_secret(
                &capture(&name, site, "bot-a"),
                &SecretValue::new(format!("value-{i:04}-xyz")),
            )
            .unwrap();
        }
        // The next one, a capture or a registered account, is refused; nothing is stored.
        let over = capture("token/example.com-aaaa1111-over", site, "bot-a");
        let r = s.capture_secret(&over, &SecretValue::new("over-value-123"));
        assert!(matches!(&r, Err(FillError::Invalid(m)) if m.contains("account_discard")));
        assert!(!s.exists("token/example.com-aaaa1111-over"));
        let r = s.register_account(&NewAccount {
            name: "login/example.com-aaaa1111",
            site,
            login_id: "me",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            label: None,
            password_len: 20,
            symbols: true,
        });
        assert!(matches!(r, Err(FillError::Invalid(_))));
        // Rotating a value it already holds is not a new entry.
        s.capture_secret(
            &capture("token/example.com-aaaa1111-k0", site, "bot-a"),
            &SecretValue::new("rotated-value-1"),
        )
        .unwrap();
        // Another bot has its own allowance; discarding frees room for the first.
        s.capture_secret(
            &capture("token/example.com-bbbb2222-k0", site, "bot-b"),
            &SecretValue::new("b-value-123456"),
        )
        .unwrap();
        s.discard_account("token/example.com-aaaa1111-k1", Some("bot-a"))
            .unwrap();
        s.capture_secret(&over, &SecretValue::new("over-value-123"))
            .unwrap();
    }

    #[test]
    fn a_capture_refuses_empty_oversized_and_unlabelled_values_and_feeds_the_redactor() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()))
            .with_value_hook(Arc::new(move |v| sink.lock().push(v.to_owned())));
        let ok = capture("token/a.example-1-k", "https://a.example", "bot-a");
        for bad in ["", "   ", &"x".repeat(MAX_CAPTURED_BYTES + 1), "a\u{7}b"] {
            let v = SecretValue::new(bad.trim());
            let r = s.capture_secret(&ok, &v);
            assert!(matches!(r, Err(FillError::Invalid(_))), "{bad:?}");
        }
        let mut no_what = ok.clone();
        no_what.what = "  ";
        assert!(
            s.capture_secret(&no_what, &SecretValue::new("value-123456"))
                .is_err()
        );
        assert!(
            s.capture_secret(
                &capture("token/a.example-1-k", "https://a.example", ""),
                &SecretValue::new("value-123456")
            )
            .is_err()
        );
        assert!(!s.exists("token/a.example-1-k"));
        assert!(seen.lock().is_empty());
        s.capture_secret(&ok, &SecretValue::new("value-123456"))
            .unwrap();
        assert_eq!(seen.lock().as_slice(), ["value-123456"]);
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
    fn owner_made_secrets_are_closed_until_the_owner_allows_a_bot() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let site = Some("https://example.com");
        s.upsert(
            "login/owner",
            Some("owner-value-123"),
            Some(&["https://example.com".to_owned()]),
            Some(&["win:sha256:abc".to_owned()]),
        )
        .unwrap();
        for bot in [Some("bot-a"), Some("bot-b"), None] {
            assert!(s.resolve_value_for("login/owner", bot).is_err());
            assert!(s.check_browser("login/owner", site, bot).is_err());
            assert!(
                s.check_desktop("login/owner", Some("win:sha256:abc"), bot)
                    .is_err()
            );
        }
        // The owner allows bot-a only: bot-b and a call without a bot stay out.
        s.upsert_with(
            "login/owner",
            FillSecretUpdate {
                bot_ids: Some(&["bot-a".to_owned()]),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        assert!(s.resolve_value_for("login/owner", Some("bot-a")).is_ok());
        assert!(s.check_browser("login/owner", site, Some("bot-a")).is_ok());
        for bot in [Some("bot-b"), None] {
            assert!(s.resolve_value_for("login/owner", bot).is_err());
            assert!(s.check_browser("login/owner", site, bot).is_err());
        }
    }

    type GapFn = Box<dyn Fn(&FillSecrets)>;

    thread_local! {
        /// Runs inside `discard_account`, after its check and before its delete.
        static GAP: std::cell::RefCell<Option<GapFn>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The seam `discard_account` calls between its check and its delete (tests only).
    pub(super) fn between_check_and_delete(s: &FillSecrets) {
        GAP.with(|g| {
            if let Some(f) = &*g.borrow() {
                f(s);
            }
        });
    }

    #[test]
    fn discard_account_holds_the_lock_from_the_check_to_the_delete() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let name = registered_account(&s);
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&held);
        GAP.with(|g| {
            *g.borrow_mut() = Some(Box::new(move |s: &FillSecrets| {
                // Another writer (the owner's delete, a re-register) would need this lock.
                seen.store(s.meta.try_lock().is_none(), Ordering::SeqCst);
            }));
        });
        s.discard_account(name, Some("bot-a")).unwrap();
        GAP.with(|g| *g.borrow_mut() = None);
        assert!(
            held.load(Ordering::SeqCst),
            "the metadata lock was released between the check and the delete"
        );
        assert!(!s.exists(name));
        assert_eq!(s.accounts(None).unwrap(), Vec::new());
    }

    #[test]
    fn a_bot_write_cannot_replace_another_bots_account() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let name = registered_account(&s); // bot-a's account
        let before = s
            .resolve_value_for(name, Some("bot-a"))
            .unwrap()
            .expose()
            .to_owned();
        let items = vec![(name.to_owned(), "chosen-by-someone-else".to_owned())];
        for bot in [Some("bot-b"), None] {
            for replace in [true, false] {
                let err = s
                    .upsert_all_for_bot(&items, &[], &[], replace, bot)
                    .unwrap_err();
                assert!(matches!(err, FillError::NotAllowed(_)), "{bot:?}: {err:?}");
            }
        }
        let still = s.resolve_value_for(name, Some("bot-a")).unwrap();
        assert_eq!(still.expose(), before, "the account value was overwritten");
        // The owning bot may ask (the owner still sees the card): the value changes,
        // the account keeps its scope.
        assert!(
            s.upsert_all_for_bot(&items, &[], &[], true, Some("bot-a"))
                .is_ok()
        );
        let own = s.list().unwrap().into_iter().find(|i| i.name == name);
        assert_eq!(own.unwrap().bot_ids, ["bot-a"]);
    }

    fn info_of(s: &FillSecrets, name: &str) -> FillSecretInfo {
        s.list()
            .unwrap()
            .into_iter()
            .find(|i| i.name == name)
            .unwrap()
    }

    /// An owner-made entry granted to bots A and C: bot B (or a call without a bot)
    /// cannot replace it through a request, with or without `replace`; value,
    /// bot list, origins and programs stay exactly as the owner left them.
    #[test]
    fn a_bot_write_cannot_take_over_an_owner_made_entry() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let (origins, programs) = (
            vec!["https://github.com".to_owned()],
            vec!["win:sha256:abc".to_owned()],
        );
        s.upsert_with(
            "login/github",
            FillSecretUpdate {
                value: Some("owner-value-123"),
                origins: Some(&origins),
                programs: Some(&programs),
                bot_ids: Some(&["bot-a".to_owned(), "bot-c".to_owned()]),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        let before = info_of(&s, "login/github");
        let items = vec![("login/github".to_owned(), "chosen-by-bot-b".to_owned())];
        let wide = vec!["https://evil.example".to_owned()];
        for bot in [Some("bot-b"), None] {
            for replace in [true, false] {
                let err = s
                    .upsert_all_for_bots(
                        &items,
                        &wide,
                        &[],
                        &["bot-b".to_owned()],
                        None,
                        replace,
                        bot,
                    )
                    .unwrap_err();
                assert!(matches!(err, FillError::NotAllowed(_)), "{bot:?}: {err:?}");
                assert!(!err.to_string().contains("chosen-by-bot-b"));
            }
        }
        assert_eq!(
            s.resolve_value_for_owner("login/github").unwrap().expose(),
            "owner-value-123"
        );
        let after = info_of(&s, "login/github");
        assert_eq!(after.bot_ids, ["bot-a", "bot-c"]);
        assert_eq!(after.origins, origins);
        assert_eq!(after.programs, programs);
        assert_eq!(after.updated_at_ms, before.updated_at_ms);
        // B cannot learn the name exists: it answers like a free name.
        assert!(!s.exists_for_bot("login/github", Some("bot-b")));
        assert!(!s.exists_for_bot("login/github", None));
        assert!(!s.exists_for_bot("login/free-name", Some("bot-b")));
        assert!(s.exists_for_bot("login/github", Some("bot-a")));
        // The owner's own write path still replaces it.
        assert!(
            s.upsert_all(
                &[("login/github".to_owned(), "owner-new-123".to_owned())],
                &origins,
                &programs,
                true,
            )
            .unwrap()
        );
    }

    /// A bot the owner allowed on an entry may replace its value; the scope the
    /// owner set (other bots, origins, programs) is kept, and the form cannot widen it.
    #[test]
    fn an_allowed_bot_replaces_the_value_and_keeps_the_scope() {
        let s = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        let origins = vec!["https://github.com".to_owned()];
        s.upsert_with(
            "login/github",
            FillSecretUpdate {
                value: Some("owner-value-123"),
                origins: Some(&origins),
                bot_ids: Some(&["bot-a".to_owned(), "bot-c".to_owned()]),
                ..FillSecretUpdate::default()
            },
        )
        .unwrap();
        let items = vec![("login/github".to_owned(), "fresh-value-456".to_owned())];
        let wide = vec!["https://evil.example".to_owned()];
        assert!(
            s.upsert_all_for_bots(
                &items,
                &wide,
                &["win:sha256:evil".to_owned()],
                &["bot-a".to_owned()],
                None,
                true,
                Some("bot-a"),
            )
            .unwrap()
        );
        assert_eq!(
            s.resolve_value_for_owner("login/github").unwrap().expose(),
            "fresh-value-456"
        );
        let after = info_of(&s, "login/github");
        assert_eq!(after.bot_ids, ["bot-a", "bot-c"]);
        assert_eq!(after.origins, origins);
        assert!(after.programs.is_empty());
        // A name nobody has yet is scoped to the requester.
        let fresh = vec![("login/new".to_owned(), "new-value-789".to_owned())];
        s.upsert_all_for_bot(&fresh, &[], &[], false, Some("bot-b"))
            .unwrap();
        assert_eq!(info_of(&s, "login/new").bot_ids, ["bot-b"]);
    }

    /// A store that "kills the daemon" (panics) right after the password is written.
    struct CrashAfterSet(Arc<MemoryStore>, std::sync::atomic::AtomicBool);
    impl SecretStore for CrashAfterSet {
        fn get(&self, n: &str) -> Result<Option<crate::SecretValue>, crate::SecretError> {
            self.0.get(n)
        }
        fn set(&self, n: &str, v: &str) -> Result<(), crate::SecretError> {
            self.0.set(n, v)?;
            assert!(
                !self.1.swap(false, Ordering::SeqCst),
                "daemon stopped after writing the password, before the metadata"
            );
            Ok(())
        }
        fn delete(&self, n: &str) -> Result<bool, crate::SecretError> {
            self.0.delete(n)
        }
        fn list_names(&self) -> Result<Vec<String>, crate::SecretError> {
            self.0.list_names()
        }
    }

    fn new_account(name: &str) -> NewAccount<'_> {
        NewAccount {
            name,
            site: "https://example.com",
            login_id: "me",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            label: None,
            password_len: 20,
            symbols: true,
        }
    }

    /// The order is: generate, write the password, write the metadata. A crash or a
    /// failure at any point must leave either nothing, or a password nobody can reach
    /// (no metadata, so no bot may use it) that the next sign-up simply replaces;
    /// never an account entry without its password.
    #[test]
    fn a_crash_between_generating_and_saving_never_leaves_a_usable_half_account() {
        let name = "login/example.com-aaaa1111";
        let key = store_key(name);
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(MemoryStore::new());

        // 1. The daemon stops after the password was stored, before the metadata.
        let crashing = Arc::new(CrashAfterSet(
            Arc::clone(&inner),
            std::sync::atomic::AtomicBool::new(true),
        ));
        let s = FillSecrets::open(crashing, dir.path()).unwrap();
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.register_account(&new_account(name))
        }));
        assert!(died.is_err(), "the simulated crash did not happen");
        drop(s);
        let orphan = inner.get(&key).unwrap().expect("the password was written");

        // 2. Restart: no account, nothing a bot can resolve or list.
        let s = FillSecrets::open(inner.clone(), dir.path()).unwrap();
        assert_eq!(s.accounts(None).unwrap(), Vec::new());
        assert_eq!(s.list().unwrap(), Vec::new());
        assert!(s.resolve_value_for(name, Some("bot-a")).is_err());
        assert!(s.discard_account(name, Some("bot-a")).is_err());

        // 3. The retry succeeds and replaces the orphan with a fresh password.
        let info = s.register_account(&new_account(name)).unwrap();
        assert!(info.has_value && info.unused);
        let fresh = s.resolve_value_for(name, Some("bot-a")).unwrap();
        assert_ne!(fresh.expose(), orphan.expose(), "the orphan was reused");
        drop(s);

        // 4. A metadata write that fails (the data folder is gone) rolls the password
        // back: no account entry exists without a password, no password without an entry.
        let dir2 = tempfile::tempdir().unwrap();
        let inner2 = Arc::new(MemoryStore::new());
        let s = FillSecrets::open(inner2.clone(), dir2.path()).unwrap();
        std::fs::remove_dir_all(dir2.path()).unwrap();
        assert!(s.register_account(&new_account(name)).is_err());
        assert!(inner2.get(&key).unwrap().is_none(), "password left behind");
        assert_eq!(s.accounts(None).unwrap(), Vec::new());
    }
}

#[cfg(test)]
mod totp_tests;
