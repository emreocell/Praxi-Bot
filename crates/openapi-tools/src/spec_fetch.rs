//! Download an `OpenAPI` document from a URL the owner typed.
//!
//! The same SSRF rules as the tools: no ambient proxy, no private or loopback
//! addresses (checked on the literal host and on every DNS answer), a size cap
//! while streaming and redirects only inside the same host.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, Url, redirect::Policy};

use crate::error::OpenApiError;
use crate::tool::{GuardResolver, private_host};

const MAX_REDIRECTS: usize = 3;

/// Fetch the text of the spec at `url` (http or https).
///
/// # Errors
/// [`OpenApiError::Fetch`] for an unusable URL, a failed request or a non-2xx
/// answer, [`OpenApiError::PrivateHost`] for a private address and
/// [`OpenApiError::SpecTooLarge`] when the body passes `max_bytes`.
pub async fn fetch_spec(
    url: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<String, OpenApiError> {
    fetch(url, max_bytes, timeout, false).await
}

/// [`fetch_spec`] with the private-address guard switched off (tests, and an
/// owner who explicitly allows a local API).
///
/// # Errors
/// See [`fetch_spec`].
pub async fn fetch_spec_allowing_private(
    url: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<String, OpenApiError> {
    fetch(url, max_bytes, timeout, true).await
}

async fn fetch(
    url: &str,
    max_bytes: usize,
    timeout: Duration,
    allow_private: bool,
) -> Result<String, OpenApiError> {
    let parsed =
        Url::parse(url.trim()).map_err(|e| OpenApiError::Fetch(format!("bad URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(OpenApiError::Fetch("only http and https URLs".into()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(OpenApiError::Fetch(
            "the URL must not contain a user name or password".into(),
        ));
    }
    if !allow_private && let Some(host) = private_host(&parsed) {
        return Err(OpenApiError::PrivateHost(host));
    }
    let origin_host = parsed.host_str().map(str::to_ascii_lowercase);
    let mut builder = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(10)));
    if !allow_private {
        builder = builder.dns_resolver(Arc::new(GuardResolver));
    }
    let client = builder
        .redirect(Policy::custom(move |attempt| {
            let same_host = attempt.url().host_str().map(str::to_ascii_lowercase) == origin_host;
            if same_host && attempt.previous().len() < MAX_REDIRECTS {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!(
            "praxi-bot-openapi-tools/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|e| OpenApiError::Fetch(e.to_string()))?;
    let mut resp = client
        .get(parsed)
        .header(
            reqwest::header::ACCEPT,
            "application/json, application/yaml, text/yaml, text/plain;q=0.8, */*;q=0.5",
        )
        .send()
        .await
        .map_err(|e| OpenApiError::Fetch(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(OpenApiError::Fetch(if status.is_redirection() {
            "the server redirects to another host".into()
        } else {
            format!("the server answered {status}")
        }));
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| OpenApiError::Fetch(e.without_url().to_string()))?
    {
        if bytes.len() + chunk.len() > max_bytes {
            return Err(OpenApiError::SpecTooLarge {
                size: bytes.len() + chunk.len(),
                limit: max_bytes,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| OpenApiError::Fetch("the document is not UTF-8 text".into()))
}
