//! "Test connection" for every key and app connection (`/v1/connection-test/*`).
//!
//! One typed result ([`ConnectionOutcome`]) for LLM provider keys, MCP servers, channel tokens,
//! web search, dictation and e-mail accounts, so a UI can show a plain-language message and a
//! concrete next step instead of a raw provider error. Every test runs on the daemon with a short
//! timeout, never returns a secret (the `details` text goes through the production redactor) and
//! never runs a tool: an MCP test starts the server and lists its tools, nothing more (a tool call
//! is an action and belongs to the permission engine).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use provider_catalog::ProviderError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::email::EmailProbeStatus;
use crate::error::{ApiError, ErrorBody};
use crate::mcp::{McpControlError, McpErrorKind, McpStatus};
use crate::routes::ApiJson;

/// Longest a single test may take before it is reported as "no network".
const TEST_TIMEOUT: Duration = Duration::from_secs(12);
/// Longest `details` text returned.
const MAX_DETAILS_CHARS: usize = 600;

/// What a connection test found. The UI maps each value to a message and a next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOutcome {
    /// The service answered and accepted the key.
    Ok,
    /// The key or token was rejected (or is missing).
    BadKey,
    /// The service could not be reached (offline, DNS, firewall, timeout).
    NoNetwork,
    /// The key works but the plan, credit or rate limit is used up.
    QuotaExceeded,
    /// The address is wrong: the server answered, but not like this service.
    WrongEndpoint,
    /// The service itself has a problem (HTTP 5xx).
    ProviderOutage,
    /// None of the above; see `details`.
    Unknown,
}

/// Result of one connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConnectionTestReport {
    /// What was tested: `provider`, `mcp`, `channel`, `web`, `voice` or `email`.
    pub target: String,
    /// The typed result.
    pub outcome: ConnectionOutcome,
    /// HTTP status the service answered with, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// The service's own error code (for example `invalid_api_key`), when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    /// Technical text for an expandable "details" box. Secret-redacted and bounded.
    pub details: String,
    /// How long the test took in milliseconds.
    pub elapsed_ms: u64,
}

impl ConnectionTestReport {
    /// A successful test.
    #[must_use]
    pub fn ok(target: &str, details: impl Into<String>) -> Self {
        Self::new(target, ConnectionOutcome::Ok, None, None, details)
    }

    /// A failed (or unclear) test.
    #[must_use]
    pub fn failed(
        target: &str,
        outcome: ConnectionOutcome,
        http_status: Option<u16>,
        provider_code: Option<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::new(target, outcome, http_status, provider_code, details)
    }

    fn new(
        target: &str,
        outcome: ConnectionOutcome,
        http_status: Option<u16>,
        provider_code: Option<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            outcome,
            http_status,
            provider_code,
            details: details.into(),
            elapsed_ms: 0,
        }
    }

    /// Redact and bound the free text, and stamp the elapsed time.
    fn finish(mut self, s: &AppState, started: Instant) -> Self {
        let text: String = self.details.chars().take(MAX_DETAILS_CHARS * 2).collect();
        let mut text = s.agent().redact_host_text(text);
        if let Some(code) = self.provider_code.take() {
            self.provider_code = Some(
                s.agent()
                    .redact_host_text(code)
                    .chars()
                    .take(80)
                    .collect::<String>(),
            );
        }
        if text.chars().count() > MAX_DETAILS_CHARS {
            text = text.chars().take(MAX_DETAILS_CHARS).collect::<String>() + "…";
        }
        self.details = text;
        self.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self
    }
}

/// Outcome for an HTTP status alone.
#[must_use]
pub fn classify_http_status(status: u16) -> ConnectionOutcome {
    match status {
        200..=299 => ConnectionOutcome::Ok,
        401 | 403 => ConnectionOutcome::BadKey,
        402 | 429 => ConnectionOutcome::QuotaExceeded,
        404 | 405 | 410 => ConnectionOutcome::WrongEndpoint,
        500..=599 => ConnectionOutcome::ProviderOutage,
        _ => ConnectionOutcome::Unknown,
    }
}

/// Best-effort outcome for a free-text error (providers that say "invalid API key" with a 400,
/// helpers that only hand back a message).
#[must_use]
pub fn classify_text(text: &str) -> ConnectionOutcome {
    let t = text.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&[
        "api key not valid",
        "api_key_invalid",
        "invalid api key",
        "invalid_api_key",
        "incorrect api key",
        "invalid x-api-key",
        "unauthorized",
        "unauthenticated",
        "forbidden",
        "authentication",
        "permission denied",
        "bad credentials",
    ]) {
        ConnectionOutcome::BadKey
    } else if has(&[
        "quota",
        "billing",
        "insufficient",
        "rate limit",
        "too many requests",
        "credit",
    ]) {
        ConnectionOutcome::QuotaExceeded
    } else if has(&[
        "timed out",
        "timeout",
        "dns",
        "connection refused",
        "unreachable",
    ]) || has(&["connection error", "connect error", "network"])
    {
        ConnectionOutcome::NoNetwork
    } else if has(&["not found", "404"]) {
        ConnectionOutcome::WrongEndpoint
    } else if has(&[
        "overloaded",
        "unavailable",
        "bad gateway",
        "internal server error",
    ]) {
        ConnectionOutcome::ProviderOutage
    } else {
        ConnectionOutcome::Unknown
    }
}

/// The service's own error code from a JSON error body (`error.code`, `error.type`,
/// `error.status`, or a top-level `code`).
fn provider_code_of(message: &str) -> Option<String> {
    let start = message.find('{')?;
    let v: serde_json::Value = serde_json::from_str(&message[start..]).ok()?;
    let err = v.get("error").unwrap_or(&v);
    ["code", "type", "status"].iter().find_map(|k| {
        let f = err.get(*k)?;
        f.as_str()
            .map(str::to_owned)
            .or_else(|| f.as_i64().map(|n| n.to_string()))
    })
}

/// Map a provider error to a report (before redaction).
#[must_use]
pub fn from_provider_error(e: &ProviderError) -> ConnectionTestReport {
    let (outcome, status, message) = match e {
        // The provider layer folds 401 and 403 into one error and drops the code.
        ProviderError::Auth(m) => (
            ConnectionOutcome::BadKey,
            None,
            format!("key rejected (HTTP 401 or 403): {m}"),
        ),
        ProviderError::RateLimited { .. } => (
            ConnectionOutcome::QuotaExceeded,
            Some(429),
            "too many requests or limit reached".to_owned(),
        ),
        ProviderError::Upstream { status, message } => (
            ConnectionOutcome::ProviderOutage,
            Some(*status),
            message.clone(),
        ),
        ProviderError::Api { status, message } => {
            let by_status = classify_http_status(*status);
            let outcome = if by_status == ConnectionOutcome::Unknown {
                // Gemini answers a bad key with a 400 and says so in the body.
                classify_text(message)
            } else {
                by_status
            };
            (outcome, Some(*status), message.clone())
        }
        ProviderError::Timeout => (ConnectionOutcome::NoNetwork, None, e.to_string()),
        ProviderError::Connection(m) => (ConnectionOutcome::NoNetwork, None, m.clone()),
        // A 200 that is not a model list: the address points at something else.
        ProviderError::Stream(m) => (ConnectionOutcome::WrongEndpoint, Some(200), m.clone()),
        other => (ConnectionOutcome::Unknown, None, other.to_string()),
    };
    let code = provider_code_of(&message);
    let details = match status {
        Some(s) => format!("HTTP {s}: {message}"),
        None => message,
    };
    ConnectionTestReport::failed("provider", outcome, status, code, details)
}

/// Body of `POST /v1/connection-test/provider`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProviderTestInput {
    /// A configured provider id (`gemini`, `openai`, `anthropic`, `local`, ...).
    pub id: String,
}

/// Body of `POST /v1/connection-test/mcp`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct McpTestInput {
    /// A configured MCP server name.
    pub name: String,
}

/// Body of `POST /v1/connection-test/channel`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ChannelTestInput {
    /// A registered channel id (`telegram`).
    pub id: String,
}

/// Body of `POST /v1/connection-test/email`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EmailTestInput {
    /// A saved e-mail account name.
    pub name: String,
}

fn too_many() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_requests",
        "too many connection tests are running; try again in a moment",
    )
}

fn check_name(what: &str, v: &str) -> Result<(), ApiError> {
    if v.is_empty() || v.len() > 200 || v.contains(['\0', '\r', '\n']) {
        return Err(ApiError::bad_request(format!(
            "`{what}` must be 1 to 200 characters without line breaks"
        )));
    }
    Ok(())
}

/// Run `fut` under the test deadline; a deadline hit is "no network".
async fn bounded<F>(target: &str, fut: F) -> ConnectionTestReport
where
    F: std::future::Future<Output = ConnectionTestReport>,
{
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| {
            ConnectionTestReport::failed(
                target,
                ConnectionOutcome::NoNetwork,
                None,
                None,
                "no answer within 12 seconds",
            )
        })
}

/// Test an LLM provider key: list the models (or, when the provider has no model list, send a
/// one-token request) and report what the provider said.
#[utoipa::path(post, path = "/v1/connection-test/provider", tag = "connection-test",
    security(("bearer" = [])), request_body = ProviderTestInput,
    responses((status = 200, body = ConnectionTestReport),
              (status = 400, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such provider"),
              (status = 429, body = ErrorBody, description = "too many tests running")))]
pub(crate) async fn test_provider(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ProviderTestInput>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    check_name("id", &input.id)?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let Some(provider) = s.agent().providers().get(&input.id) else {
        // A key that was saved but never produced a working provider (for example a CLI engine,
        // or a key that failed to load) has nothing to call.
        let report = if input.id.starts_with("cli:") {
            ConnectionTestReport::ok(
                "provider",
                "this is a local command-line engine; it signs in on its own and has no key",
            )
        } else {
            return Err(ApiError::not_found(format!("provider `{}`", input.id)));
        };
        return Ok(Json(report.finish(&s, started)));
    };
    let default_model =
        provider_catalog::ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
            .ok()
            .and_then(|c| c.get(&input.id).and_then(|p| p.default_model.clone()));
    let report = bounded("provider", async {
        match provider.list_models().await {
            Ok(models) if !models.is_empty() => {
                ConnectionTestReport::ok("provider", format!("{} models available", models.len()))
            }
            Ok(_) => match default_model {
                Some(m) => one_token(&provider, &input.id, &m).await,
                None => ConnectionTestReport::ok(
                    "provider",
                    "the service has no model list to check; the key itself was not verified",
                ),
            },
            Err(e) => from_provider_error(&e),
        }
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

/// The cheapest real request: one token from the default model.
async fn one_token(
    provider: &Arc<dyn provider_catalog::ChatProvider>,
    id: &str,
    model: &str,
) -> ConnectionTestReport {
    use futures::StreamExt;
    use provider_catalog::{ChatRequest, Message, MessageRole, ModelRef};
    let mut req = ChatRequest::new(
        ModelRef(format!("{id}/{model}")),
        vec![Message::text(MessageRole::User, "hi")],
    );
    req.max_tokens = Some(1);
    match provider.stream(req).await {
        Ok(mut stream) => match stream.next().await {
            Some(Err(e)) => from_provider_error(&e),
            _ => ConnectionTestReport::ok("provider", "the model answered a one-word test"),
        },
        Err(e) => from_provider_error(&e),
    }
}

/// Test an MCP server: (re)start it and list its tools. No tool is called.
#[utoipa::path(post, path = "/v1/connection-test/mcp", tag = "connection-test",
    security(("bearer" = [])), request_body = McpTestInput,
    responses((status = 200, body = ConnectionTestReport),
              (status = 404, body = ErrorBody), (status = 429, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn test_mcp(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<McpTestInput>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    check_name("name", &input.name)?;
    let host = s
        .mcp()
        .ok_or_else(|| ApiError::unavailable("MCP management is not available in this daemon"))?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let report = bounded("mcp", async {
        match host.reconnect(&input.name).await {
            Err(McpControlError::NotFound(_)) => ConnectionTestReport::failed(
                "mcp",
                ConnectionOutcome::Unknown,
                None,
                None,
                "server not found",
            ),
            Err(e) => ConnectionTestReport::failed(
                "mcp",
                ConnectionOutcome::Unknown,
                None,
                None,
                format!("{e:?}"),
            ),
            Ok(dto) if !dto.enabled => ConnectionTestReport::failed(
                "mcp",
                ConnectionOutcome::Unknown,
                None,
                None,
                "the server is switched off",
            ),
            Ok(dto) => match dto.status {
                McpStatus::Connected => match host.tools(&input.name) {
                    Ok(tools) => ConnectionTestReport::ok(
                        "mcp",
                        format!("started; {} tools listed (none were run)", tools.len()),
                    ),
                    Err(e) => ConnectionTestReport::failed(
                        "mcp",
                        ConnectionOutcome::Unknown,
                        None,
                        None,
                        format!("{e:?}"),
                    ),
                },
                McpStatus::AuthRequired => ConnectionTestReport::failed(
                    "mcp",
                    ConnectionOutcome::BadKey,
                    None,
                    None,
                    "the server needs a sign-in",
                ),
                _ => {
                    let text = dto.error.clone().unwrap_or_default();
                    let mut details = text.clone();
                    if !dto.stderr_tail.is_empty() {
                        details = format!("{details}\n{}", dto.stderr_tail.join("\n"));
                    }
                    let outcome = match dto.error_kind {
                        Some(
                            McpErrorKind::RuntimeMissing
                            | McpErrorKind::Spawn
                            | McpErrorKind::Config
                            | McpErrorKind::NeedsFolder,
                        ) => ConnectionOutcome::WrongEndpoint,
                        Some(McpErrorKind::Timeout) => ConnectionOutcome::NoNetwork,
                        _ => classify_text(&text),
                    };
                    ConnectionTestReport::failed("mcp", outcome, None, None, details)
                }
            },
        }
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

/// Test a channel token (Telegram: `getMe`).
#[utoipa::path(post, path = "/v1/connection-test/channel", tag = "connection-test",
    security(("bearer" = [])), request_body = ChannelTestInput,
    responses((status = 200, body = ConnectionTestReport),
              (status = 404, body = ErrorBody, description = "no such channel"), (status = 429, body = ErrorBody)))]
pub(crate) async fn test_channel(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ChannelTestInput>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    check_name("id", &input.id)?;
    let channel = s
        .channels()
        .into_iter()
        .find(|c| c.info().id == input.id)
        .ok_or_else(|| ApiError::not_found(format!("channel `{}`", input.id)))?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let report = bounded("channel", async {
        channel.test_connection().await.unwrap_or_else(|| {
            ConnectionTestReport::failed(
                "channel",
                ConnectionOutcome::Unknown,
                None,
                None,
                "this channel has no connection test",
            )
        })
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

/// Test the web-search keys: one live probe through the configured backends.
#[utoipa::path(post, path = "/v1/connection-test/web", tag = "connection-test",
    security(("bearer" = [])),
    responses((status = 200, body = ConnectionTestReport), (status = 429, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "web search management is not available")))]
pub(crate) async fn test_web(
    State(s): State<AppState>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    let control = s.web().ok_or_else(|| {
        ApiError::unavailable("web search management is not available in this daemon")
    })?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let report = bounded("web", async {
        match control
            .test(crate::web::WebSearchTestInput {
                query: None,
                backend: None,
            })
            .await
        {
            Ok(r) if r.ok => ConnectionTestReport::ok(
                "web",
                format!(
                    "{} answered with {} results",
                    r.backend.as_deref().unwrap_or("a backend"),
                    r.result_count
                ),
            ),
            Ok(r) => {
                let mut details = r.error.clone().unwrap_or_default();
                for e in &r.fallback_errors {
                    details = format!("{details}\n{e}");
                }
                let status = first_http_status(&details);
                let outcome = status
                    .map(classify_http_status)
                    .filter(|o| *o != ConnectionOutcome::Unknown)
                    .unwrap_or_else(|| classify_text(&details));
                ConnectionTestReport::failed("web", outcome, status, None, details)
            }
            Err(e) => ConnectionTestReport::failed(
                "web",
                ConnectionOutcome::Unknown,
                None,
                None,
                format!("{e:?}"),
            ),
        }
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

/// A three-digit HTTP status mentioned in free text ("... 401 Unauthorized").
fn first_http_status(text: &str) -> Option<u16> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|w| w.len() == 3)
        .filter_map(|w| w.parse::<u16>().ok())
        .find(|n| (400..600).contains(n))
}

/// Test dictation: send half a second of silence to the configured speech service.
#[utoipa::path(post, path = "/v1/connection-test/voice", tag = "connection-test",
    security(("bearer" = [])),
    responses((status = 200, body = ConnectionTestReport), (status = 429, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "dictation is not available")))]
pub(crate) async fn test_voice(
    State(s): State<AppState>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    use crate::voice::{VoiceControlError, VoiceProvider};
    let control = s
        .voice()
        .ok_or_else(|| ApiError::unavailable("dictation is not available in this daemon"))?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let cfg = control.get();
    let (mime, audio) = match cfg.stt_provider {
        VoiceProvider::GeminiLive => ("audio/pcm", vec![0u8; 16_000]),
        _ => ("audio/wav", silent_wav()),
    };
    let report = bounded("voice", async {
        match control.transcribe(audio, mime.into(), None, false).await {
            Ok(_) => ConnectionTestReport::ok("voice", "the speech service accepted a test clip"),
            Err(VoiceControlError::NotConfigured) => ConnectionTestReport::failed(
                "voice",
                ConnectionOutcome::BadKey,
                None,
                None,
                "dictation is not set up yet",
            ),
            Err(VoiceControlError::Invalid(m) | VoiceControlError::Internal(m)) => {
                ConnectionTestReport::failed("voice", ConnectionOutcome::Unknown, None, None, m)
            }
            Err(VoiceControlError::Upstream(m)) => {
                let status = first_http_status(&m);
                let outcome = status
                    .map(classify_http_status)
                    .filter(|o| *o != ConnectionOutcome::Unknown)
                    .unwrap_or_else(|| classify_text(&m));
                ConnectionTestReport::failed("voice", outcome, status, provider_code_of(&m), m)
            }
        }
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

/// 0.5 s of 16 kHz mono 16-bit silence as a WAV file.
fn silent_wav() -> Vec<u8> {
    let data_len: u32 = 16_000;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&16_000u32.to_le_bytes());
    v.extend_from_slice(&32_000u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(44 + data_len as usize, 0);
    v
}

/// Test a saved e-mail account (including `AgentMail` API accounts).
#[utoipa::path(post, path = "/v1/connection-test/email", tag = "connection-test",
    security(("bearer" = [])), request_body = EmailTestInput,
    responses((status = 200, body = ConnectionTestReport),
              (status = 404, body = ErrorBody), (status = 429, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn test_email(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<EmailTestInput>,
) -> Result<Json<ConnectionTestReport>, ApiError> {
    check_name("name", &input.name)?;
    let control = s.email().ok_or_else(|| {
        ApiError::unavailable("e-mail management is not available in this daemon")
    })?;
    let _permit = s.connection_test_permit().ok_or_else(too_many)?;
    let started = Instant::now();
    let report = bounded("email", async {
        match control.test_saved(&input.name).await {
            Ok(r) => {
                let main = r.api.unwrap_or(if r.imap == EmailProbeStatus::Ok {
                    r.smtp
                } else {
                    r.imap
                });
                ConnectionTestReport::failed(
                    "email",
                    email_outcome(main),
                    None,
                    None,
                    format!("imap: {:?}, smtp: {:?}, api: {:?}", r.imap, r.smtp, r.api),
                )
            }
            Err(e) => ConnectionTestReport::failed(
                "email",
                ConnectionOutcome::Unknown,
                None,
                None,
                format!("{e:?}"),
            ),
        }
    })
    .await;
    Ok(Json(report.finish(&s, started)))
}

fn email_outcome(s: EmailProbeStatus) -> ConnectionOutcome {
    match s {
        EmailProbeStatus::Ok => ConnectionOutcome::Ok,
        EmailProbeStatus::AuthFailed => ConnectionOutcome::BadKey,
        EmailProbeStatus::Unreachable | EmailProbeStatus::Timeout => ConnectionOutcome::NoNetwork,
        EmailProbeStatus::Tls | EmailProbeStatus::Protocol => ConnectionOutcome::WrongEndpoint,
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn classifies_by_status_and_text() {
        assert_eq!(classify_http_status(401), ConnectionOutcome::BadKey);
        assert_eq!(classify_http_status(429), ConnectionOutcome::QuotaExceeded);
        assert_eq!(classify_http_status(404), ConnectionOutcome::WrongEndpoint);
        assert_eq!(classify_http_status(503), ConnectionOutcome::ProviderOutage);
        assert_eq!(
            classify_text("API key not valid. Please pass a valid API key."),
            ConnectionOutcome::BadKey
        );
        assert_eq!(first_http_status("backend failed: HTTP 401 x"), Some(401));
    }
}
