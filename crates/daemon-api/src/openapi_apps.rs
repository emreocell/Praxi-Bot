//! REST APIs described by an `OpenAPI` document (`/v1/openapi-apps/*`): the
//! DTOs and the [`OpenApiAppsControl`] trait the embedder implements (see
//! `apps/cli/src/openapi_host.rs`).  The daemon holds no state of its own.
//!
//! Everything here is owner-only (`OWNER_ROUTES`, and no paired role except
//! the owner's desktop).  Credentials are write-only: the stored secret is
//! never returned, only `secret_set`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How the API is authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenApiAuthKind {
    /// No credentials.
    None,
    /// An API key in a request header (`name` is the header).
    ApiKeyHeader,
    /// An API key in the query string (`name` is the parameter).
    ApiKeyQuery,
    /// `Authorization: Bearer <secret>`.
    Bearer,
    /// HTTP basic auth (`username` plus the secret as the password).
    Basic,
}

/// How an operation changes things, from its HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenApiEffect {
    /// GET / HEAD / OPTIONS.
    Read,
    /// POST / PUT / PATCH.
    Write,
    /// DELETE.
    Destructive,
}

/// The credentials of an API as stored (no secret value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OpenApiAuthDto {
    /// Kind of credential.
    pub kind: OpenApiAuthKind,
    /// Header or query parameter name (API key kinds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// User name (basic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// A secret is stored (its value is never returned).
    pub secret_set: bool,
}

/// One operation found in the document.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OpenApiOperationDto {
    /// Tool name a bot sees.
    pub name: String,
    /// HTTP method (upper case).
    pub method: String,
    /// Path template.
    pub path: String,
    /// First line of the operation's summary (third-party text).
    pub summary: String,
    /// Tags of the operation.
    pub tags: Vec<String>,
    /// Effect class from the method.
    pub effect: OpenApiEffect,
    /// Money-like (pay, charge, checkout, ...): asks on every autonomy level.
    pub always_ask: bool,
    /// At least one bot may use it.
    pub enabled: bool,
    /// Bots that may use it; `null` = every bot. Meaningful when `enabled`.
    #[serde(default)]
    pub bots: Option<Vec<String>>,
}

/// An operation the importer left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OpenApiSkippedDto {
    /// HTTP method.
    pub method: String,
    /// Path template.
    pub path: String,
    /// Why it was left out.
    pub reason: String,
}

/// A connected API (`GET /v1/openapi-apps`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OpenApiAppDto {
    /// Short id (also the tool-name prefix: `api_<id>_<operation>`).
    pub id: String,
    /// Display name chosen by the owner.
    pub name: String,
    /// `info.title` of the document (third-party text).
    pub title: String,
    /// `info.version` of the document.
    pub version: String,
    /// The only origin and path prefix requests go to.
    pub base_url: String,
    /// Where the document came from again on refresh; absent when it was pasted or uploaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_url: Option<String>,
    /// Credentials.
    pub auth: OpenApiAuthDto,
    /// Operations found, in document order.
    pub operations: Vec<OpenApiOperationDto>,
    /// Operations left out, with the reason.
    pub skipped: Vec<OpenApiSkippedDto>,
    /// Notes from the import (unresolved references and similar).
    pub warnings: Vec<String>,
    /// Authentication the document declares, for a hint to the owner.
    pub security_hints: Vec<String>,
    /// How many operations are enabled for at least one bot.
    pub enabled_count: u32,
    /// Largest number of operations that can be enabled at once.
    pub max_enabled: u32,
}

/// Credentials in a request. `secret` is write-only.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenApiAuthInput {
    /// Kind of credential.
    pub kind: OpenApiAuthKind,
    /// Header or query parameter name (API key kinds).
    #[serde(default)]
    pub name: Option<String>,
    /// User name (basic).
    #[serde(default)]
    pub username: Option<String>,
    /// The key, token or password. Omit it to keep the stored one.
    #[serde(default)]
    pub secret: Option<String>,
}

impl std::fmt::Debug for OpenApiAuthInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenApiAuthInput")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("username", &self.username)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Body of `POST /v1/openapi-apps`. Give the document as `spec_url` or `spec_text`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenApiAppInput {
    /// Display name.
    pub name: String,
    /// Where to download the document (http or https, public address).
    #[serde(default)]
    pub spec_url: Option<String>,
    /// The document itself (JSON or YAML), for example read from a file.
    #[serde(default)]
    pub spec_text: Option<String>,
    /// The API's base URL. Defaults to the first `servers` entry of the document.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Credentials; default none.
    #[serde(default)]
    pub auth: Option<OpenApiAuthInput>,
}

/// One enabled operation in an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenApiEnabledOperation {
    /// Tool name of the operation.
    pub name: String,
    /// Bots that may use it; `null` = every bot, `[]` = none.
    #[serde(default)]
    pub bots: Option<Vec<String>>,
}

/// Body of `PUT /v1/openapi-apps/{id}`; omitted fields keep their value.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenApiAppUpdate {
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// New base URL.
    #[serde(default)]
    pub base_url: Option<String>,
    /// New credentials (`secret` omitted keeps the stored secret).
    #[serde(default)]
    pub auth: Option<OpenApiAuthInput>,
    /// The complete set of enabled operations; anything not listed is off.
    #[serde(default)]
    pub operations: Option<Vec<OpenApiEnabledOperation>>,
}

/// Failures of the embedder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenApiAppsError {
    /// No such app (`404`).
    NotFound(String),
    /// Invalid input, a document that cannot be used or a refused address (`422`).
    Invalid(String),
    /// The document could not be downloaded (`502`).
    Upstream(String),
    /// Persistence or secret-store failure (`500`).
    Internal(String),
}

/// What `/v1/openapi-apps/*` needs from the embedder: persisting the config,
/// keeping credentials in the secret store and (re)registering the tools.
#[async_trait]
pub trait OpenApiAppsControl: Send + Sync {
    /// The connected APIs.
    fn list(&self) -> Vec<OpenApiAppDto>;

    /// One API.
    ///
    /// # Errors
    /// [`OpenApiAppsError::NotFound`].
    fn get(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError>;

    /// Download or read the document, store the credential and save the app.
    /// Every operation starts disabled.
    ///
    /// # Errors
    /// [`OpenApiAppsError`].
    async fn add(&self, input: OpenApiAppInput) -> Result<OpenApiAppDto, OpenApiAppsError>;

    /// Change an app and apply it to the registered tools at once.
    ///
    /// # Errors
    /// [`OpenApiAppsError`].
    async fn update(
        &self,
        id: &str,
        input: OpenApiAppUpdate,
    ) -> Result<OpenApiAppDto, OpenApiAppsError>;

    /// Download the document again (apps added from a URL) and keep the enabled
    /// operations that still exist.
    ///
    /// # Errors
    /// [`OpenApiAppsError`].
    async fn refresh(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError>;

    /// Remove an app, its tools and its stored credential.
    ///
    /// # Errors
    /// [`OpenApiAppsError::NotFound`].
    async fn remove(&self, id: &str) -> Result<(), OpenApiAppsError>;

    /// Drop a deleted bot from every operation's bot list (an emptied list
    /// stays `[]`, it never becomes "all bots"). Returns the number of apps changed.
    ///
    /// # Errors
    /// [`OpenApiAppsError::Internal`] when the change cannot be saved.
    async fn forget_bot(&self, bot_id: &str) -> Result<usize, OpenApiAppsError>;
}
