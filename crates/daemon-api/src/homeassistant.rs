//! Home Assistant setup (`/v1/homeassistant*`): DTOs and the [`HomeAssistantControl`] trait the
//! embedder implements (`apps/cli/src/homeassistant_host.rs`).
//!
//! The long-lived access token is **write-only**: it enters through [`HomeAssistantInput`] or
//! [`HomeAssistantTestInput`], goes straight into the secret store (or is used once for a test),
//! and no response or log line ever carries it (responses only report `token_set`). Every route is
//! owner-only: the address and the token decide what a bot can do to the owner's home.

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// The saved Home Assistant setup (non-secret fields only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeAssistantDto {
    /// Whether a hub is set up at all.
    pub configured: bool,
    /// Address of the hub (`null` when not set up).
    pub url: Option<String>,
    /// Whether an access token is stored (never returned).
    pub token_set: bool,
    /// Services (`cover.open_cover`) bots may call without a card even though they open,
    /// unlock or disarm something.
    pub trusted_services: Vec<String>,
    /// The bots that may use Home Assistant; `null` = every bot, an empty list = none.
    pub bots: Option<Vec<String>>,
}

/// Body of `PUT /v1/homeassistant`: the address and token are checked against the hub before
/// anything is saved.
#[derive(Clone, Deserialize, ToSchema)]
pub struct HomeAssistantInput {
    /// Address of the hub, e.g. `http://homeassistant.local:8123`.
    pub url: String,
    /// Long-lived access token, **write-only**. Required the first time and whenever the address
    /// changes; omitted otherwise it keeps the stored one.
    #[serde(default)]
    pub token: Option<String>,
    /// Services bots may call without a card even though they open, unlock or disarm something:
    /// exact `domain.service` names. Empty = none.
    #[serde(default)]
    pub trusted_services: Vec<String>,
}

impl std::fmt::Debug for HomeAssistantInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeAssistantInput")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("trusted_services", &self.trusted_services)
            .finish()
    }
}

/// Result of `PUT /v1/homeassistant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeAssistantSaved {
    /// What is saved now.
    pub settings: HomeAssistantDto,
    /// How many entities the hub reported when it was checked.
    pub entities: u32,
}

/// Body of `PUT /v1/homeassistant/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantBotsInput {
    /// The bots that may use Home Assistant; `null` = every bot, an empty list = none.
    /// The field must be present: an empty body or a misspelled name is refused instead of
    /// silently granting every bot.
    #[serde(deserialize_with = "present_or_null")]
    #[schema(required = true)]
    pub bots: Option<Vec<String>>,
}

/// Like `Option<T>`, but a missing key is an error (serde's default for a bare `Option` field
/// would turn it into `None`).
fn present_or_null<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// Body of `POST /v1/homeassistant/test`: the saved hub (an empty body), or an address and
/// token that are not saved yet (`url` with `token`). Never a mix: the stored token is only
/// ever sent to the saved address.
#[derive(Clone, Default, Deserialize, ToSchema)]
pub struct HomeAssistantTestInput {
    /// Address to try.
    #[serde(default)]
    pub url: Option<String>,
    /// Access token, **write-only**, used once.
    #[serde(default)]
    pub token: Option<String>,
}

impl std::fmt::Debug for HomeAssistantTestInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeAssistantTestInput")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Result of `POST /v1/homeassistant/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeAssistantTestReport {
    /// `true` when the hub was reachable and accepted the token.
    pub ok: bool,
    /// How many entities the hub reports (`null` when the test failed).
    pub entities: Option<u32>,
    /// What went wrong, in plain words (`null` on success).
    pub error: Option<String>,
}

/// Why a [`HomeAssistantControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeAssistantControlError {
    /// Nothing is set up (`404`).
    NotFound(String),
    /// Invalid input, or a hub that refused the token (`422`).
    Invalid(String),
    /// Persistence / secret-store failure (`500`).
    Internal(String),
}

/// What `/v1/homeassistant*` needs from the embedder: persisting
/// `<data dir>/homeassistant.toml`, writing the token to the secret store, reloading the
/// registered `ha_*` tools and probing a hub.
#[async_trait]
pub trait HomeAssistantControl: Send + Sync {
    /// The saved setup.
    fn get(&self) -> HomeAssistantDto;

    /// Check the hub, then save it and use it from the next tool call (no restart).
    ///
    /// # Errors
    /// [`HomeAssistantControlError`].
    async fn save(
        &self,
        input: HomeAssistantInput,
    ) -> Result<HomeAssistantSaved, HomeAssistantControlError>;

    /// Choose which bots may use Home Assistant.
    ///
    /// # Errors
    /// [`HomeAssistantControlError::NotFound`] when nothing is set up.
    async fn set_bots(
        &self,
        bots: Option<Vec<String>>,
    ) -> Result<HomeAssistantDto, HomeAssistantControlError>;

    /// Remove the setup and its stored token.
    ///
    /// # Errors
    /// [`HomeAssistantControlError::NotFound`] when nothing is set up.
    async fn remove(&self) -> Result<(), HomeAssistantControlError>;

    /// Try the saved hub, or an address and token that are not saved yet. A hub that cannot be
    /// reached or refuses the token is a report with `ok: false`, not an error.
    ///
    /// # Errors
    /// [`HomeAssistantControlError::Invalid`] for input that cannot be tried at all,
    /// [`HomeAssistantControlError::NotFound`] when the saved hub is asked for and none is set up.
    async fn test(
        &self,
        input: HomeAssistantTestInput,
    ) -> Result<HomeAssistantTestReport, HomeAssistantControlError>;
}
