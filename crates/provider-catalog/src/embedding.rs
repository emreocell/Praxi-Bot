//! Bounded embedding requests, independent of chat capabilities.
//!
//! An invocation performs exactly one HTTP attempt. The knowledge worker owns
//! consent, durable quota reservations, retries, and cancellation. These adapters
//! never log request bodies and never follow redirects with document text.

use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::{Response, Url, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelRef, OpenAiCompatConfig, ProviderError, limits, util};

pub const MAX_EMBEDDING_INPUTS: usize = 64;
pub const MAX_EMBEDDING_DIMENSIONS: usize = 8192;
pub const MAX_EMBEDDING_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_EMBEDDING_BATCH_BYTES: usize = 1024 * 1024;

/// The two halves of asymmetric retrieval must use the matching model recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingTask {
    Document,
    Query,
}

/// Explicit wire semantics; a model name alone does not describe task handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingTaskFormat {
    /// Native Gemini `taskType` (`gemini-embedding-001`).
    Gemini001,
    /// Gemini Embedding 2's documented query/document text prefixes.
    Gemini2,
    /// A standard OpenAI-compatible embedding endpoint; no task parameter.
    Plain,
    /// Local multilingual E5: `query: ` and `passage: ` prefixes.
    E5,
}

/// One input. Debug output deliberately omits its text and title.
#[derive(Clone)]
pub struct EmbeddingInput {
    pub text: String,
    pub title: Option<String>,
}

impl fmt::Debug for EmbeddingInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingInput")
            .field("bytes", &self.text.len())
            .field("has_title", &self.title.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub model: ModelRef,
    pub inputs: Vec<EmbeddingInput>,
    /// Required output dimension, validated even when not sent to the server.
    pub dimensions: usize,
    /// Some OpenAI-compatible models do not accept a `dimensions` parameter.
    pub request_dimensions: bool,
    pub task: EmbeddingTask,
    pub format: EmbeddingTaskFormat,
}

impl EmbeddingRequest {
    /// Check request bounds before JSON serialization or network activity.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for an invalid model, input, or size.
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.model.0.is_empty()
            || self.model.0.len() > 512
            || self.model.0.chars().any(char::is_control)
            || self.inputs.is_empty()
            || self.inputs.len() > MAX_EMBEDDING_INPUTS
            || !(1..=MAX_EMBEDDING_DIMENSIONS).contains(&self.dimensions)
        {
            return Err(invalid("invalid embedding model, batch size, or dimension"));
        }
        let mut size = 0usize;
        for input in &self.inputs {
            let title_len = input.title.as_ref().map_or(0, String::len);
            if input.text.trim().is_empty()
                || input.text.len() > MAX_EMBEDDING_INPUT_BYTES
                || title_len > 4096
            {
                return Err(invalid(
                    "embedding input is empty or exceeds its byte limit",
                ));
            }
            size = size
                .saturating_add(input.text.len())
                .saturating_add(title_len);
        }
        if size > MAX_EMBEDDING_BATCH_BYTES {
            return Err(invalid("embedding batch exceeds its byte limit"));
        }
        Ok(())
    }

    pub(crate) fn model_for<'a>(&'a self, provider: &str) -> &'a str {
        if self.model.provider() == Some(provider) {
            self.model.model()
        } else {
            &self.model.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingResponse {
    /// Vectors in the exact order of the request inputs.
    pub vectors: Vec<Vec<f32>>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
}

/// A provider instance uses the same credentials and endpoint as its catalog
/// entry. Its caller must authorize all egress before calling `embed`.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn id(&self) -> &str;
    /// Canonical endpoint identity, without secrets, for consent/cache binding.
    ///
    /// # Errors
    /// Invalid or credential-bearing endpoint configuration.
    fn endpoint_identity(&self) -> Result<String, ProviderError>;
    fn is_remote(&self) -> bool {
        true
    }
    /// Exactly one attempt. Dropping this future stops local IO but cannot
    /// retract bytes already sent to a server or undo provider charges.
    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, ProviderError>;
    async fn list_embedding_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(Vec::new())
    }
}

/// Canonicalize a configured base URL for consent and generation identity.
///
/// # Errors
/// Rejects non-HTTP endpoints and URLs carrying userinfo, query, or fragment.
pub fn endpoint_identity(endpoint: &str) -> Result<String, ProviderError> {
    let url = Url::parse(endpoint).map_err(|_| invalid("invalid embedding endpoint URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "embedding endpoint must be HTTP(S) without URL credentials or query",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| canonical(ip).is_loopback())
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

/// Link-local, metadata, unspecified, multicast: never allowed.
fn always_blocked(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_unspecified() || v6.is_multicast() || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Private LAN, ULA, CGNAT and reserved ranges.
fn is_private_ip(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] == 0
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfec0
        }
    }
}

/// May an address reached through `host` receive embedding text and keys?
/// Loopback only when the host is spelled as loopback (so a public name that
/// resolves to 127.0.0.1 is refused).
fn address_allowed(host: &str, ip: IpAddr, allow_local_network: bool) -> bool {
    if always_blocked(ip) {
        return false;
    }
    if canonical(ip).is_loopback() {
        return is_loopback_host(host);
    }
    allow_local_network || !is_private_ip(ip)
}

/// Check an embedding endpoint the owner typed: `https` only, except plain
/// `http` to a loopback host (Ollama); no credentials; no link-local or cloud
/// metadata address; no private address unless `allow_local_network`.
///
/// # Errors
/// A plain-language description of what is not allowed.
pub fn validate_endpoint(
    endpoint: &str,
    allow_local_network: bool,
) -> Result<String, ProviderError> {
    let identity = endpoint_identity(endpoint)?;
    let url = Url::parse(&identity).map_err(|_| invalid("invalid embedding endpoint URL"))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = is_loopback_host(host);
    if url.scheme() == "http" && !loopback {
        return Err(invalid(
            "plain http is only allowed for localhost; use https",
        ));
    }
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        && !address_allowed(host, ip, allow_local_network)
    {
        return Err(invalid(
            "this address is link-local, private or reserved and is not allowed",
        ));
    }
    Ok(identity)
}

/// Resolves names and refuses answers that point at forbidden addresses, so a
/// public-looking name cannot be rebound to the metadata service or the LAN.
struct GuardedResolver {
    allow_local_network: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow = self.allow_local_network;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let found = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let addrs: Vec<std::net::SocketAddr> = found
                .filter(|a| address_allowed(&host, a.ip(), allow))
                .collect();
            if addrs.is_empty() {
                return Err("embedding endpoint resolves to a forbidden address".into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub(crate) fn client(
    config: &OpenAiCompatConfig,
    base_url: &str,
) -> Result<reqwest::Client, ProviderError> {
    validate_endpoint(base_url, config.allow_local_network)?;
    reqwest::Client::builder()
        .dns_resolver(std::sync::Arc::new(GuardedResolver {
            allow_local_network: config.allow_local_network,
        }))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.timeout)
        .timeout(config.timeout)
        .build()
        .map_err(|_| ProviderError::Connection("cannot create embedding HTTP client".into()))
}

pub(crate) fn invalid(message: &str) -> ProviderError {
    ProviderError::InvalidRequest(message.to_owned())
}

pub(crate) async fn json_response(
    response: Response,
    classify: util::Classify,
) -> Result<Value, ProviderError> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return limits::response_json(response).await;
    }
    let headers = response.headers().clone();
    let body = limits::error_body(response).await.unwrap_or_default();
    // Classifiers may inspect provider RetryInfo but never expose server-echoed
    // document text, credentials, or arbitrary error strings to logs/UI.
    let error = classify(status, &headers, &body);
    Err(match error {
        ProviderError::RateLimited { retry_after } => ProviderError::RateLimited {
            retry_after: util::retry_after(&headers)
                .or_else(|| retry_after_date(&headers))
                .or_else(|| retry_info(&body))
                .or(retry_after)
                .map(|duration| duration.min(Duration::from_secs(172_800))),
        },
        ProviderError::Auth(_) => ProviderError::Auth("embedding authorization failed".into()),
        ProviderError::Upstream { status, .. } => ProviderError::Upstream {
            status,
            message: "embedding provider unavailable".into(),
        },
        _ => ProviderError::Api {
            status,
            message: "embedding request rejected".into(),
        },
    })
}

fn retry_info(body: &str) -> Option<Duration> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/details")?
        .as_array()?
        .iter()
        .filter_map(|detail| {
            detail
                .get("retryDelay")?
                .as_str()?
                .strip_suffix('s')?
                .parse::<f64>()
                .ok()
        })
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.)
        // Preserve legitimate daily quota resets, but a malformed proxy must
        // not create an effectively permanent host-wide cooldown.
        .map(|seconds| Duration::from_secs_f64(seconds.min(172_800.)))
        .max()
}

fn retry_after_date(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    )
}

pub(crate) fn vector(value: &Value, dimensions: usize) -> Result<Vec<f32>, ProviderError> {
    let values = value
        .as_array()
        .filter(|values| values.len() == dimensions)
        .ok_or_else(|| invalid("embedding response dimension mismatch"))?;
    values
        .iter()
        .map(|value| {
            let value = value
                .as_f64()
                .ok_or_else(|| invalid("embedding response contains a non-number"))?;
            #[allow(clippy::cast_possible_truncation)]
            let value = value as f32;
            if value.is_finite() {
                Ok(value)
            } else {
                Err(invalid("embedding response contains a non-finite number"))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
