//! Composio app connections (`/v1/composio/*`): DTOs and the
//! [`ComposioControl`] trait the embedder implements (see
//! `apps/cli/src/composio_host.rs`).  The daemon holds no Composio state.
//!
//! Catalog and logo reads require authentication. Account identities, action
//! previews and credential installation also require owner proof. Phone,
//! node and host peers are refused for every method.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Connection state of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposioConnState {
    /// Nothing connected.
    NotConnected,
    /// Sign-in is not finished.
    Pending,
    /// Connected.
    Active,
    /// The authorization ran out; reconnect.
    Expired,
    /// The sign-in did not finish (cancelled, refused or timed out).
    Failed,
}

/// How Praxi is linked to Composio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposioLinkMode {
    /// No Composio account linked.
    None,
    /// Linked through the browser login (no key was typed or copied).
    Login,
    /// The owner's own Composio project key.
    ProjectKey,
}

/// `POST /v1/composio/login`: the one-time browser login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioLoginDto {
    /// https URL on Composio's dashboard; open it in the system browser.
    pub login_url: String,
    /// When the login expires (ISO timestamp, may be empty).
    pub expires_at: String,
}

/// State of the one-time browser login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposioLoginState {
    /// No login was started.
    Idle,
    /// Waiting for the user in the browser.
    Pending,
    /// Linked.
    Linked,
    /// Expired, refused or failed; start again.
    Failed,
}

/// `GET /v1/composio/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioLoginStatusDto {
    /// State of the login.
    pub state: ComposioLoginState,
}

/// `GET /v1/composio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioStatusDto {
    /// Praxi is linked to Composio (the credential itself is never returned).
    pub configured: bool,
    /// How it is linked.
    pub mode: ComposioLinkMode,
    /// Where the owner can create a Composio project key.
    pub key_setup_url: String,
    /// Composio refused the saved credential (401/403) and nothing renewed it:
    /// the owner has to sign in to Composio again, a retry cannot succeed.
    #[serde(default)]
    pub login_expired: bool,
    /// Display name of the linked organization or project.
    pub account: Option<String>,
}

/// Body of `PUT /v1/composio/key`.  Write-only.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposioKeyInput {
    /// The Composio project API key.
    pub value: String,
}

impl std::fmt::Debug for ComposioKeyInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposioKeyInput(<redacted>)")
    }
}

/// One toolkit of the Composio catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioToolkitDto {
    /// Toolkit slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Short description (third-party text).
    pub description: String,
    /// Category names.
    pub categories: Vec<String>,
    /// Number of actions, when Composio reports it.
    pub tools_count: Option<u64>,
    /// Chip group: `communication`, `social`, `productivity`, `dev`,
    /// `finance`, `business` or `other`.
    pub group: String,
    /// Logo URL (https), when Composio gives one.
    pub logo: Option<String>,
    /// Praxi connects it directly (always `false` until a zero-setup direct
    /// path exists); the UI notes it but never blocks the choice.
    pub native_first: bool,
    /// State for this installation.
    pub state: ComposioConnState,
    /// The owner enabled it.
    pub enabled: bool,
}

/// `GET /v1/composio/toolkits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioCatalogDto {
    /// The page: popular apps first, then Composio's popularity order.
    pub items: Vec<ComposioToolkitDto>,
    /// Cursor of the next page.
    pub next_cursor: Option<String>,
    /// `bundled` (only the apps shipped with Praxi so far) or `cache`.
    pub source: String,
    /// Unix seconds of the disk cache, when there is one.
    pub updated_at: Option<u64>,
    /// A background refresh is running.
    pub refreshing: bool,
    /// Why the last refresh failed: `offline`, `unauthorized`,
    /// `rate_limited` or `failed`.  The list above is still usable.
    pub error: Option<String>,
}

/// Query of `GET /v1/composio/toolkits`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ComposioCatalogQuery {
    /// Text matched against name, slug and description.
    pub search: Option<String>,
    /// Chip group (`all` or empty = every group).
    pub group: Option<String>,
    /// Cursor from the previous page.
    pub cursor: Option<String>,
    /// Page size, 1 to 100 (default 48).
    pub limit: Option<u32>,
}

/// A connected (or connecting) app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioAppDto {
    /// Toolkit slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// State.
    pub state: ComposioConnState,
    /// The owner enabled it (tools exist only while enabled and active).
    pub enabled: bool,
    /// Praxi connects it directly.
    pub native_first: bool,
    /// Bots allowed; `null` = all.
    pub bots: Option<Vec<String>>,
    /// Direct tools currently offered to the model.
    pub tools: u32,
    /// Display identity of the bound account, when available.
    pub account: Option<String>,
}

/// A catalog action available after connecting a toolkit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioActionDto {
    /// Action name, scoped to the toolkit.
    pub name: String,
    /// Short description from Composio.
    pub description: String,
}

/// `POST /v1/composio/apps/{slug}/connect`: the one-time consent link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioConnectDto {
    /// https URL on Composio's domain; open it in the system browser.
    pub redirect_url: String,
    /// When the link expires (ISO timestamp, may be empty).
    pub expires_at: String,
}

/// Body of `PUT /v1/composio/apps/{slug}/enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposioEnabledInput {
    /// New value.
    pub enabled: bool,
}

/// Body of `PUT /v1/composio/apps/{slug}/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposioBotsInput {
    /// The bots that may use the app; `null` = every bot, an empty list = none.
    /// The field must be present: an empty body or a misspelled name is refused
    /// instead of silently granting every bot.
    #[serde(deserialize_with = "present_or_null")]
    #[schema(required = true)]
    pub bots: Option<Vec<String>>,
}

/// Like `Option<T>`, but a missing key is an error (serde's default for a bare
/// `Option` field would turn it into `None`).
fn present_or_null<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// `POST /v1/composio/apps/{slug}/test`: result of the authorization check or read-only probe.
/// Shown to the owner only; the text comes from the app and is never trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioTestDto {
    /// Only the active authorization record was checked.
    #[serde(default)]
    pub authorization_only: bool,
    /// The account the app is connected as (address or user name), when the app says.
    pub account: Option<String>,
    /// Up to three samples: file names, mail subjects, calendar names.
    pub items: Vec<String>,
}

/// `DELETE /v1/composio/apps/{slug}` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComposioDisconnectDto {
    /// The account was deleted on Composio's side too.  When `false` the app
    /// is forgotten locally and the owner can remove it in Composio's dashboard.
    pub remote_removed: bool,
}

/// Why a [`ComposioControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposioControlError {
    /// Unknown or not connected app (`404`).
    NotFound(String),
    /// Invalid input (`422`).
    Invalid(String),
    /// Not linked to Composio yet (`409`).
    NotConfigured,
    /// Composio no longer accepts the saved login or key (`409`): sign in again.
    LoginExpired,
    /// Composio rate limit (`429`).
    RateLimited(String),
    /// Composio failed or is unreachable (`502`).
    Upstream(String),
    /// Local failure (`500`).
    Internal(String),
}

/// A validated app logo (see `composio::logo`).
#[derive(Debug, Clone)]
pub struct ComposioLogo {
    pub bytes: Vec<u8>,
    /// `image/png`, `image/webp` or `image/svg+xml`.
    pub content_type: &'static str,
}

/// What `/v1/composio/*` needs from the embedder.
#[async_trait]
pub trait ComposioControl: Send + Sync {
    /// Link status.
    async fn status(&self) -> ComposioStatusDto;
    /// Start the one-time browser login.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn login_start(&self) -> Result<ComposioLoginDto, ComposioControlError>;
    /// One check of the browser login.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn login_poll(&self) -> Result<ComposioLoginStatusDto, ComposioControlError>;
    /// Validate and store the key.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn set_key(&self, key: &str) -> Result<(), ComposioControlError>;
    /// Delete the key.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn remove_key(&self) -> Result<(), ComposioControlError>;
    /// One catalog page, from the local cache (never waits for the network).
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn catalog(
        &self,
        query: ComposioCatalogQuery,
    ) -> Result<ComposioCatalogDto, ComposioControlError>;
    /// List the toolkit's catalog actions using the same filter as bot discovery.
    ///
    /// # Errors
    /// Unsupported toolkit or failed discovery.
    async fn actions(&self, slug: &str) -> Result<Vec<ComposioActionDto>, ComposioControlError>;
    /// The cached or freshly fetched logo of an app; `Ok(None)` when none is known.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn logo(&self, _slug: &str) -> Result<Option<ComposioLogo>, ComposioControlError> {
        Ok(None)
    }
    /// Apps the owner connected or started to connect.
    fn apps(&self) -> Vec<ComposioAppDto>;
    /// Refresh and return one app.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn app(&self, slug: &str) -> Result<ComposioAppDto, ComposioControlError>;
    /// Start connecting an app.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn connect(&self, slug: &str) -> Result<ComposioConnectDto, ComposioControlError>;
    /// Switch an app on or off.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn set_enabled(
        &self,
        slug: &str,
        enabled: bool,
    ) -> Result<ComposioAppDto, ComposioControlError>;
    /// Disconnect an app.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn disconnect(&self, slug: &str) -> Result<ComposioDisconnectDto, ComposioControlError>;
    /// Check the bound authorization and run a fixed read-only probe where available.
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn test(&self, _slug: &str) -> Result<ComposioTestDto, ComposioControlError> {
        Err(ComposioControlError::Invalid(
            "this build has no connection test".into(),
        ))
    }
    /// Choose which bots may use an app (`None` = all).
    ///
    /// # Errors
    /// [`ComposioControlError`].
    async fn set_bots(
        &self,
        _slug: &str,
        _bots: Option<Vec<String>>,
    ) -> Result<ComposioAppDto, ComposioControlError> {
        Err(ComposioControlError::Invalid(
            "this build cannot limit an app to some bots".into(),
        ))
    }
}
