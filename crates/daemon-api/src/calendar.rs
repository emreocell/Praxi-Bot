//! Calendar account management (`/v1/calendar/*`): DTOs and the
//! [`CalendarControl`] trait the embedder implements (`apps/cli/src/calendar_host.rs`).
//!
//! The (app) password is **write-only**: it enters through
//! [`CalendarAccountInput`] or [`CalendarTestInput`], goes straight into the
//! secret store (or is used once for a test), and no response or log line
//! ever carries it (responses only report `password_set`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One calendar account (non-secret fields only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CalendarAccountDto {
    /// Short name bots use to pick the calendar (`personal`).
    pub name: String,
    /// Address of the calendar (a `CalDAV` collection).
    pub url: String,
    /// Login name.
    pub username: String,
    /// Address invitations are sent from; `null` uses the login when it is an address.
    pub email: Option<String>,
    /// Whether a password is stored (never returned).
    pub password_set: bool,
}

/// Body of `POST /v1/calendar/accounts` and `PUT /v1/calendar/accounts/{name}`.
#[derive(Clone, Deserialize, ToSchema)]
pub struct CalendarAccountInput {
    /// Short name (letters, digits, `-` and `_`). Ignored by `PUT`: the path names it.
    #[serde(default)]
    pub name: String,
    /// Address of the calendar. A server address that holds several calendars
    /// is accepted when it holds exactly one.
    pub url: String,
    /// Login name.
    pub username: String,
    /// (App) password, **write-only**. Required to create; with `PUT` omitted keeps the stored one.
    #[serde(default)]
    pub password: Option<String>,
    /// Address invitations are sent from. Empty clears it.
    #[serde(default)]
    pub email: Option<String>,
}

impl std::fmt::Debug for CalendarAccountInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalendarAccountInput")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("email", &self.email)
            .finish()
    }
}

/// Body of `POST /v1/calendar/test`: either a saved account (`name`) or a login
/// that is not saved yet (`url`, `username`, `password`). Never a mix: a saved
/// account is always tried at its own address.
#[derive(Clone, Default, Deserialize, ToSchema)]
pub struct CalendarTestInput {
    /// A saved account to test.
    #[serde(default)]
    pub name: Option<String>,
    /// Address to try.
    #[serde(default)]
    pub url: Option<String>,
    /// Login name.
    #[serde(default)]
    pub username: Option<String>,
    /// (App) password, **write-only**, used once.
    #[serde(default)]
    pub password: Option<String>,
}

impl std::fmt::Debug for CalendarTestInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalendarTestInput")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A calendar a test found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CalendarFoundDto {
    /// Name the owner gave it.
    pub name: String,
    /// Its address: what an account's `url` should be.
    pub url: String,
}

/// Result of `POST /v1/calendar/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CalendarTestReport {
    /// `true` when the server accepted the login and was reachable.
    pub ok: bool,
    /// The calendars found.
    pub calendars: Vec<CalendarFoundDto>,
    /// Whether the server sends invitations itself (`CalDAV` scheduling).
    pub scheduling: bool,
    /// What went wrong, in plain words (`null` on success).
    pub error: Option<String>,
}

/// Why a [`CalendarControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarControlError {
    /// No such account (`404`).
    NotFound(String),
    /// The name is taken (`409`).
    Conflict(String),
    /// Invalid input, or a server that refused the login (`422`).
    Invalid(String),
    /// Persistence / secret-store failure (`500`).
    Internal(String),
}

/// What `/v1/calendar/*` needs from the embedder: persisting
/// `<data dir>/calendar.toml`, writing passwords to the secret store,
/// reloading the registered `calendar_*` tools and probing a server.
#[async_trait]
pub trait CalendarControl: Send + Sync {
    /// The saved accounts.
    fn list(&self) -> Vec<CalendarAccountDto>;

    /// Add an account and use it from the next tool call.
    ///
    /// # Errors
    /// [`CalendarControlError`].
    async fn create(
        &self,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError>;

    /// Change an account.
    ///
    /// # Errors
    /// [`CalendarControlError`].
    async fn update(
        &self,
        name: &str,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError>;

    /// Remove an account and its stored password.
    ///
    /// # Errors
    /// [`CalendarControlError::NotFound`].
    async fn delete(&self, name: &str) -> Result<(), CalendarControlError>;

    /// Try a login and list its calendars. A server that cannot be reached or
    /// refuses the login is a report with `ok: false`, not an error.
    ///
    /// # Errors
    /// [`CalendarControlError::Invalid`] for input that cannot be tried at all.
    async fn test(
        &self,
        input: CalendarTestInput,
    ) -> Result<CalendarTestReport, CalendarControlError>;
}
