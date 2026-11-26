//! Plain HTTPS client for Composio's REST API.
//!
//! API reference (read 2026-11-15, v3.1):
//! - <https://docs.composio.dev/reference/api-reference/tools/postToolsExecuteByToolSlug>
//! - <https://docs.composio.dev/reference/api-reference/connected-accounts/postConnectedAccountsLink>
//! - <https://docs.composio.dev/reference/api-reference/connected-accounts/getConnectedAccounts>
//! - <https://docs.composio.dev/reference/api-reference/connected-accounts/deleteConnectedAccountsByNanoid>
//! - <https://docs.composio.dev/reference/api-reference/auth-configs/postAuthConfigs>
//! - <https://docs.composio.dev/reference/api-reference/auth-configs/getAuthConfigs>
//!
//! Security properties: the origin is fixed (`https://backend.composio.dev`);
//! every URL is built from it, no URL from a response is ever requested; no
//! redirect is followed; the key sits in a default header marked sensitive;
//! upstream error bodies are replaced by fixed status messages; responses are capped at 1 MiB.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::redirect::Policy;
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use url::Url;

use crate::error::{ComposioError, Result};

/// The one origin Praxi talks to.
pub const API_ORIGIN: &str = "https://backend.composio.dev";
/// Base URL of the API version in use.
pub const API_BASE: &str = "https://backend.composio.dev/api/v3.1";

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_CHARS: usize = 300;

/// Masks secrets in text (the daemon's live redactor).
pub type Redact = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// State of one connected account as Composio reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Connected account id (never leaves the daemon).
    pub id: String,
    /// Toolkit slug.
    pub toolkit: String,
    /// Raw status (`ACTIVE`, `INITIATED`, `EXPIRED`, ...).
    pub status: String,
    /// Optional owner-facing alias. Never read from provider credential state.
    pub label: Option<String>,
}

/// Non-secret fields of the Composio browser session.
pub(crate) struct SessionInfo {
    pub ids: crate::state::LoginIds,
    pub label: Option<String>,
}

/// A fresh connect link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Validated https URL on Composio's domain.
    pub redirect_url: String,
    /// Account created by this connect flow; daemon-local, never in the DTO.
    pub(crate) account_id: String,
    /// ISO timestamp.
    pub expires_at: String,
}

/// One toolkit of Composio's catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Toolkit {
    /// Toolkit slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Description (untrusted text, capped).
    pub description: String,
    /// Category names.
    pub categories: Vec<String>,
    /// Number of tools, when reported.
    pub tools_count: Option<u64>,
    /// Logo URL (https only), when Composio gives one.
    #[serde(default)]
    pub logo: Option<String>,
}

/// One page of the catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolkitPage {
    /// Toolkits in the order Composio returned them.
    pub items: Vec<Toolkit>,
    /// Cursor of the next page.
    pub next_cursor: Option<String>,
}

/// What Composio's catalog says about one app slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolkitLookup {
    /// An ordinary app toolkit.
    Found,
    /// The slug exists but is not an ordinary app toolkit (never connected).
    Unsupported,
    /// The search did not list it.
    Missing,
}

/// One tool of a toolkit, as Composio describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTool {
    /// Composio tool slug (`GITHUB_CREATE_AN_ISSUE`).
    pub slug: String,
    /// Parent toolkit as returned by discovery.
    pub toolkit: String,
    /// Concrete action version.
    pub version: String,
    /// Description (untrusted, capped).
    pub description: String,
    /// Sanitized JSON schema of the inputs.
    pub schema: Value,
}

/// How the client authenticates.
#[derive(Clone)]
pub enum Auth {
    /// A project API key (`x-api-key`): the owner's own Composio project.
    Project(String),
    /// The user key obtained by the browser login (`x-user-api-key`), with the
    /// org and project it belongs to (`x-org-id`, `x-project-id`).
    User {
        /// The user key.
        key: String,
        /// Organization id (not a secret).
        org_id: Option<String>,
        /// Project id (not a secret).
        project_id: Option<String>,
    },
    /// No credential: only the browser-login endpoints.
    Anonymous,
}

/// A browser-login session at Composio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSession {
    /// Session id (goes into the dashboard URL as `cliKey`).
    pub id: String,
    /// ISO timestamp, may be empty.
    pub expires_at: String,
}

/// Result of polling a browser-login session.
#[derive(Clone, PartialEq, Eq)]
pub enum CliPoll {
    /// The user has not finished in the browser.
    Pending,
    /// Finished: the user key.  Never logged (see the `Debug` impl).
    Linked(String),
}

impl std::fmt::Debug for CliPoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Linked(_) => f.write_str("Linked(<key hidden>)"),
        }
    }
}

/// Composio REST client.  Built once per credential.
#[derive(Clone)]
pub struct ComposioClient {
    http: Client,
    base: Url,
    redact: Redact,
    label: Arc<tokio::sync::OnceCell<Option<String>>>,
}

impl std::fmt::Debug for ComposioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposioClient(<key hidden>)")
    }
}

/// Whether `url` is an https URL without credentials on Composio's domain.
#[must_use]
pub fn is_composio_https_url(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let hosted_link = rest
        .strip_prefix("app.composio.dev/link/")
        .and_then(|path| path.split(['?', '#']).next())
        .is_some_and(valid_id);
    !url.chars().any(|c| c.is_control() || c == '\\')
        && (matches!(authority, "connect.composio.dev" | "backend.composio.dev") || hosted_link)
        && u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && u.host_str() == Some(authority)
}

impl ComposioClient {
    /// Client for the production origin.
    ///
    /// # Errors
    /// The key is not a valid header value or the HTTP client cannot be built.
    ///
    /// # Panics
    /// Never: the base URL is a constant.
    pub fn new(api_key: &str, redact: Redact) -> Result<Self> {
        Self::new_with(&Auth::Project(api_key.to_owned()), redact)
    }

    /// Client for the production origin with an explicit credential kind.
    ///
    /// # Errors
    /// A credential is not a valid header value or the HTTP client cannot be built.
    ///
    /// # Panics
    /// Never: the base URL is a constant.
    pub fn new_with(auth: &Auth, redact: Redact) -> Result<Self> {
        Self::build(auth, Url::parse(API_BASE).expect("constant"), redact, true)
    }

    /// Client for a fake server in tests.  Accepts only a loopback `http` URL;
    /// there is no way to point the client at any other host.
    ///
    /// # Errors
    /// `base` is not a loopback URL.
    pub fn for_loopback_tests(api_key: &str, base: &str, redact: Redact) -> Result<Self> {
        Self::for_loopback_with(&Auth::Project(api_key.to_owned()), base, redact)
    }

    /// [`Self::for_loopback_tests`] with an explicit credential kind.
    ///
    /// # Errors
    /// `base` is not a loopback URL.
    pub fn for_loopback_with(auth: &Auth, base: &str, redact: Redact) -> Result<Self> {
        let url = Url::parse(base).map_err(|e| ComposioError::Internal(e.to_string()))?;
        let loopback = url.host_str().is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !loopback || url.scheme() != "http" {
            return Err(ComposioError::Internal(
                "the test base URL must be http on loopback".into(),
            ));
        }
        Self::build(auth, url, redact, false)
    }

    fn build(auth: &Auth, base: Url, redact: Redact, https_only: bool) -> Result<Self> {
        let secret = |v: &str| -> Result<HeaderValue> {
            let mut h = HeaderValue::from_str(v)
                .map_err(|_| ComposioError::Invalid("the API key has invalid characters".into()))?;
            h.set_sensitive(true);
            Ok(h)
        };
        let plain = |v: &str| -> Result<HeaderValue> {
            HeaderValue::from_str(v)
                .map_err(|_| ComposioError::Invalid("invalid Composio identifier".into()))
        };
        let mut headers = HeaderMap::new();
        match auth {
            Auth::Project(key) => {
                headers.insert(HeaderName::from_static("x-api-key"), secret(key)?);
            }
            Auth::User {
                key,
                org_id,
                project_id,
            } => {
                headers.insert(HeaderName::from_static("x-user-api-key"), secret(key)?);
                // Composio wants the NANO ids here (`proj_...`); a UUID is refused
                // with a 401/403 on every call, so such a pair is never sent (the
                // key alone addresses the developer project).
                if let (Some(org), Some(project)) = (org_id, project_id)
                    && !looks_like_uuid(org)
                    && !looks_like_uuid(project)
                {
                    headers.insert(HeaderName::from_static("x-org-id"), plain(org)?);
                    headers.insert(HeaderName::from_static("x-project-id"), plain(project)?);
                }
            }
            Auth::Anonymous => {}
        }
        let http = Client::builder()
            .default_headers(headers)
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .https_only(https_only)
            .user_agent(concat!("praxi-bot-composio/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ComposioError::Internal(format!("http client: {e}")))?;
        Ok(Self {
            http,
            base,
            redact,
            label: Arc::default(),
        })
    }

    /// `base` + path segments, percent-encoded.  Segments are checked by the
    /// callers to be `[A-Za-z0-9_-]`.
    fn url(&self, segments: &[&str]) -> Url {
        let mut u = self.base.clone();
        {
            let mut s = u.path_segments_mut().expect("http base has a path");
            s.pop_if_empty();
            for seg in segments {
                s.push(seg);
            }
        }
        debug_assert_eq!(u.origin(), self.base.origin());
        u
    }

    fn clean(&self, text: &str) -> String {
        let masked = (self.redact)(text);
        let mut out: String = masked.chars().take(MAX_ERROR_CHARS).collect();
        if masked.chars().count() > MAX_ERROR_CHARS {
            out.push('…');
        }
        out
    }

    fn display_label(&self, value: Option<&Value>) -> Option<String> {
        let label = (self.redact)(value?.as_str()?);
        let label = crate::schema::clean_text(label.trim(), 120);
        (!label.is_empty()).then_some(label)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Value> {
        self.send_capped(req, MAX_RESPONSE_BYTES).await
    }

    async fn send_capped(&self, req: RequestBuilder, cap: usize) -> Result<Value> {
        let mut resp = req
            .send()
            .await
            .map_err(|e| ComposioError::Transport(self.clean(&e.without_url().to_string())))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ComposioError::Transport(self.clean(&e.without_url().to_string())))?
        {
            if body.len() + chunk.len() > cap {
                return Err(ComposioError::Upstream(
                    "the response is larger than the allowed size".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match status {
            s if s.is_success() => Ok(json),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ComposioError::Unauthorized),
            StatusCode::TOO_MANY_REQUESTS => Err(ComposioError::RateLimited {
                retry_after_secs: retry_after,
            }),
            s => Err(ComposioError::Upstream(format!(
                "Composio request failed ({})",
                s.as_u16()
            ))),
        }
    }

    /// Cheap authenticated GET used to validate a key.
    ///
    /// # Errors
    /// [`ComposioError::Unauthorized`] for a rejected key.
    pub async fn validate_key(&self) -> Result<()> {
        let req = self
            .http
            .request(Method::GET, self.url(&["auth_configs"]))
            .query(&[("limit", "1")]);
        self.send(req).await.map(|_| ())
    }

    /// The Composio-managed auth config of `toolkit`, created when missing.
    ///
    /// # Errors
    /// Transport or API errors.
    pub async fn ensure_auth_config(&self, toolkit: &str) -> Result<String> {
        let list = self
            .http
            .request(Method::GET, self.url(&["auth_configs"]))
            .query(&[
                ("toolkit_slug", toolkit),
                ("is_composio_managed", "true"),
                ("limit", "1"),
            ]);
        let v = self.send(list).await?;
        if let Some(id) = v
            .pointer("/items/0/id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Ok(id.to_owned());
        }
        let create = self
            .http
            .request(Method::POST, self.url(&["auth_configs"]))
            .json(&json!({
                "toolkit": {"slug": toolkit},
                "auth_config": {"type": "use_composio_managed_auth"}
            }));
        let v = self.send(create).await?;
        v.pointer("/auth_config/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ComposioError::Upstream("no auth config id in the response".into()))
    }

    /// Create a hosted connect link for `user_id`.
    ///
    /// # Errors
    /// API errors, or a `redirect_url` that is not https on Composio's domain.
    pub async fn create_link(&self, auth_config_id: &str, user_id: &str) -> Result<Link> {
        let req = self
            .http
            .request(Method::POST, self.url(&["connected_accounts", "link"]))
            .json(&json!({"auth_config_id": auth_config_id, "user_id": user_id}));
        let v = self.send(req).await?;
        parse_link(&v)
    }

    /// Start a browser login at Composio: no credential needed.  The user
    /// then opens `https://dashboard.composio.dev/?cliKey=<id>`.
    ///
    /// UNVERIFIED against the live API: the path comes from the Composio CLI
    /// (`composio login`, `ts/packages/cli/src/effects/browser-login.ts`) and
    /// its mock server (`POST /api/v3.1/cli/create-session`, answer
    /// `{id, code, expiresAt, status}`).
    ///
    /// # Errors
    /// API or transport errors.
    pub async fn create_cli_session(&self) -> Result<CliSession> {
        let req = self
            .http
            .request(Method::POST, self.url(&["cli", "create-session"]))
            .json(&json!({"scope": "user"}));
        let v = self.send(req).await?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .ok_or_else(|| ComposioError::Upstream("no login session id in the response".into()))?;
        Ok(CliSession {
            id: id.to_owned(),
            expires_at: v
                .get("expiresAt")
                .or_else(|| v.get("expires_at"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// Poll a browser-login session.
    ///
    /// UNVERIFIED against the live API: `GET /api/v3.1/cli/get-session?id=`
    /// answering `{status: "pending" | "linked", api_key}` (the CLI reads
    /// `status` and `api_key` of a linked session, `models/session.ts`).
    ///
    /// # Errors
    /// API or transport errors (an expired session answers an error).
    pub async fn get_cli_session(&self, id: &str) -> Result<CliPoll> {
        if !valid_id(id) {
            return Err(ComposioError::Invalid("invalid login session".into()));
        }
        let req = self
            .http
            .request(Method::GET, self.url(&["cli", "get-session"]))
            .query(&[("id", id)]);
        let v = self.send(req).await?;
        match (
            v.get("status").and_then(Value::as_str),
            v.get("api_key").and_then(Value::as_str),
        ) {
            (Some("linked"), Some(key)) if !key.is_empty() => Ok(CliPoll::Linked(key.to_owned())),
            _ => Ok(CliPoll::Pending),
        }
    }

    /// Org and project of the logged-in user key.
    ///
    /// `GET /auth/session/info` with `x-user-api-key`.  The CLI reads the org
    /// from `project.org.id` and the project from `project.nano_id`
    /// (`effects/browser-login.ts`): `project.id` is a UUID, which Composio
    /// refuses in the `x-project-id` header.  A UUID is never returned here.
    ///
    /// # Errors
    /// API or transport errors.
    pub(crate) async fn session_info(&self) -> Result<SessionInfo> {
        let req = self
            .http
            .request(Method::GET, self.url(&["auth", "session", "info"]));
        let v = self.send(req).await?;
        let pick = |p: &str| {
            v.pointer(p)
                .and_then(Value::as_str)
                .filter(|s| valid_id(s) && !looks_like_uuid(s))
                .map(str::to_owned)
        };
        Ok(SessionInfo {
            ids: crate::state::LoginIds {
                org_id: pick("/project/org/id"),
                project_id: pick("/project/nano_id").or_else(|| pick("/project/id")),
            },
            label: [
                "/project/org/name",
                "/org_member/name",
                "/org_member/email",
                "/project/name",
            ]
            .into_iter()
            .find_map(|path| self.display_label(v.pointer(path))),
        })
    }

    pub(crate) async fn project_label(&self) -> Result<Option<String>> {
        let req = self
            .http
            .request(Method::GET, self.url(&["org", "project", "config"]));
        let v = self.send(req).await?;
        Ok(self.display_label(v.get("display_name")))
    }

    pub(crate) async fn account_label(&self, browser_login: bool) -> Option<String> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.label.get_or_try_init(|| async {
                if browser_login {
                    self.session_info().await.map(|info| info.label)
                } else {
                    self.project_label().await
                }
            }),
        )
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .cloned()
        .flatten()
    }

    /// A Tool Router session for `user_id` (used to create connect links when
    /// logged in with a user key; Composio-managed OAuth needs no auth config).
    ///
    /// # Errors
    /// API or transport errors.
    pub async fn create_router_session(&self, user_id: &str) -> Result<String> {
        let req = self
            .http
            .request(Method::POST, self.url(&["tool_router", "session"]))
            .json(&json!({
                "user_id": user_id,
                "manage_connections": {"enable": true}
            }));
        let v = self.send(req).await?;
        v.get("session_id")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .map(str::to_owned)
            .ok_or_else(|| ComposioError::Upstream("no session id in the response".into()))
    }

    /// A connect link for `toolkit` inside a Tool Router session.
    ///
    /// # Errors
    /// API errors, or a `redirect_url` outside Composio's domain.
    pub async fn router_link(&self, session_id: &str, toolkit: &str) -> Result<Link> {
        if !valid_id(session_id) {
            return Err(ComposioError::Invalid("invalid session".into()));
        }
        let req = self
            .http
            .request(
                Method::POST,
                self.url(&["tool_router", "session", session_id, "link"]),
            )
            .json(&json!({"toolkit": toolkit}));
        let v = self.send(req).await?;
        parse_link(&v)
    }

    /// Connected accounts of `user_id`, optionally of one toolkit.
    ///
    /// UNVERIFIED: array query parameters (`user_ids`, `toolkit_slugs`) are sent
    /// as a single value; the docs do not say whether several values are
    /// repeated or comma-joined, and one value works either way.
    ///
    /// # Errors
    /// Transport or API errors.
    pub async fn list_accounts(
        &self,
        user_id: &str,
        toolkit: Option<&str>,
    ) -> Result<Vec<Account>> {
        let mut q = vec![("user_ids", user_id), ("limit", "100")];
        if let Some(t) = toolkit {
            q.push(("toolkit_slugs", t));
        }
        let req = self
            .http
            .request(Method::GET, self.url(&["connected_accounts"]))
            .query(&q);
        let v = self.send(req).await?;
        if v.get("next_cursor")
            .is_some_and(|c| !c.is_null() && c != "")
        {
            return Err(ComposioError::Upstream(
                "account list is incomplete; account selection refused".into(),
            ));
        }
        let items = v
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ComposioError::Upstream("invalid account list".into()))?;
        items
            .iter()
            .map(|i| {
                let parse = || {
                    let status = i.get("status")?.as_str()?;
                    let status =
                        if status == "ACTIVE" && i.get("is_disabled") == Some(&Value::Bool(true)) {
                            "INACTIVE"
                        } else {
                            status
                        };
                    Some(Account {
                        id: i.get("id")?.as_str()?.to_owned(),
                        toolkit: i.pointer("/toolkit/slug")?.as_str()?.to_owned(),
                        status: status.to_owned(),
                        label: self.display_label(i.get("alias")),
                    })
                };
                let account = parse()
                    .ok_or_else(|| ComposioError::Upstream("invalid account metadata".into()))?;
                if account.id.is_empty()
                    || !matches!(
                        account.status.as_str(),
                        "ACTIVE"
                            | "INITIALIZING"
                            | "INITIATED"
                            | "EXPIRED"
                            | "INACTIVE"
                            | "FAILED"
                            | "REVOKED"
                    )
                {
                    return Err(ComposioError::Upstream(
                        "unsupported account metadata".into(),
                    ));
                }
                Ok(account)
            })
            .collect()
    }

    /// One page of the toolkit catalog, most used first.
    ///
    /// UNVERIFIED: what a `category` value looks like (the docs list no enum;
    /// items carry `meta.categories` as objects whose field names are not
    /// documented, so a string or an object with `name` / `slug` is read);
    /// whether `sort_by=usage` is the order of the website's popularity list.
    ///
    /// # Errors
    /// Transport or API errors.
    pub async fn list_toolkits(
        &self,
        search: Option<&str>,
        category: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ToolkitPage> {
        let limit = limit.clamp(1, 100).to_string();
        let mut q = vec![("sort_by", "usage"), ("limit", limit.as_str())];
        if let Some(x) = search.filter(|s| !s.is_empty()) {
            q.push(("search", x));
        }
        if let Some(x) = category.filter(|s| !s.is_empty()) {
            q.push(("category", x));
        }
        if let Some(x) = cursor.filter(|s| !s.is_empty()) {
            q.push(("cursor", x));
        }
        let req = self
            .http
            .request(Method::GET, self.url(&["toolkits"]))
            .query(&q);
        let v = self.send(req).await?;
        let items = v
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(parse_toolkit).collect())
            .unwrap_or_default();
        Ok(ToolkitPage {
            items,
            next_cursor: v
                .get("next_cursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        })
    }

    /// Check the exact toolkit without depending on catalog search ranking.
    /// The v3.1 list remains a fallback for deployments that only expose the
    /// toolkit there. An explicit unsupported kind is always refused.
    ///
    /// # Errors
    /// Transport or API errors.
    pub async fn lookup_toolkit(&self, slug: &str) -> Result<ToolkitLookup> {
        let exact = self
            .http
            .request(Method::GET, self.url(&["toolkits", slug]));
        match self.send(exact).await {
            Ok(found) => {
                return Ok(
                    if found.get("slug").and_then(Value::as_str) == Some(slug)
                        && parse_toolkit(&found).is_some()
                    {
                        ToolkitLookup::Found
                    } else {
                        ToolkitLookup::Unsupported
                    },
                );
            }
            Err(ComposioError::Upstream(m)) if m == "Composio request failed (404)" => {}
            Err(e) => return Err(e),
        }
        let req = self
            .http
            .request(Method::GET, self.url(&["toolkits"]))
            .query(&[("sort_by", "usage"), ("limit", "100"), ("search", slug)]);
        let v = self.send(req).await?;
        let found = v
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|i| {
                i.get("slug")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(slug))
            });
        Ok(match found {
            None => ToolkitLookup::Missing,
            Some(i) if parse_toolkit(i).is_some() => ToolkitLookup::Found,
            Some(_) => ToolkitLookup::Unsupported,
        })
    }

    /// The tools of one toolkit, in Composio's order.
    ///
    /// Pages include input schemas and are capped at 8 MiB each.
    ///
    /// # Errors
    /// Transport or API errors.
    pub async fn list_tools(&self, toolkit: &str) -> Result<Vec<RemoteTool>> {
        const CAP: usize = 8 * 1024 * 1024;
        let mut tools = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cursor = None;
        // Bound the total work as well as the size of each upstream response.
        for _ in 0..30 {
            let req = self
                .http
                .request(Method::GET, self.url(&["tools"]))
                .query(&[("toolkit_slug", toolkit), ("limit", "100")]);
            let req = match cursor.as_deref() {
                Some(c) => req.query(&[("cursor", c)]),
                None => req,
            };
            let v = self.send_capped(req, CAP).await?;
            let items = v
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| ComposioError::Upstream("invalid action list".into()))?;
            tools.extend(items.iter().filter_map(parse_tool).map(|mut tool| {
                tool.description = (self.redact)(&tool.description);
                tool
            }));
            let Some(next) = v
                .get("next_cursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                return Ok(tools);
            };
            if !seen.insert(next.to_owned()) {
                break;
            }
            cursor = Some(next.to_owned());
        }
        Err(ComposioError::Upstream("action list is incomplete".into()))
    }

    /// Delete a connected account (its Composio-side tokens).
    ///
    /// # Errors
    /// API errors; `404` counts as already deleted.
    pub async fn delete_account(&self, id: &str) -> Result<()> {
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || id.is_empty()
        {
            return Err(ComposioError::Upstream("unexpected account id".into()));
        }
        let req = self
            .http
            .request(Method::DELETE, self.url(&["connected_accounts", id]));
        match self.send(req).await {
            Ok(_) => Ok(()),
            Err(ComposioError::Upstream(m)) if m.ends_with("(404)") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Execute one tool for `user_id`.  Returns the `data` of the response.
    ///
    /// # Errors
    /// API errors; `successful: false` becomes [`ComposioError::Upstream`].
    pub async fn execute(
        &self,
        tool_slug: &str,
        user_id: &str,
        version: &str,
        account_id: &str,
        arguments: &Value,
    ) -> Result<Value> {
        if tool_slug.is_empty()
            || !tool_slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(ComposioError::Invalid("bad tool slug".into()));
        }
        let req = self
            .http
            .request(Method::POST, self.url(&["tools", "execute", tool_slug]))
            .json(&execute_body(user_id, version, account_id, arguments));
        let v = self.send(req).await?;
        if v.get("successful").and_then(Value::as_bool) == Some(false) {
            return Err(ComposioError::Upstream(
                "Composio action failed (200)".into(),
            ));
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Whether `id` has the 8-4-4-4-12 shape of a UUID (Composio's nano ids never do).
#[must_use]
pub fn looks_like_uuid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A connect-link answer: https on Composio's domain, plus the account id.
fn parse_link(v: &Value) -> Result<Link> {
    let url = v
        .get("redirect_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The URL is only ever handed to the owner's browser, never requested
    // here, and only when it is on Composio's own domain.
    if !is_composio_https_url(url) {
        return Err(ComposioError::Upstream(
            "Composio returned a connect link outside its own domain; it was discarded".into(),
        ));
    }
    let account_id = v
        .get("connected_account_id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or_else(|| {
            ComposioError::Upstream(
                "no valid account binding in connect response; reconnect required".into(),
            )
        })?;
    Ok(Link {
        account_id: account_id.to_owned(),
        redirect_url: url.to_owned(),
        expires_at: v
            .get("expires_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn parse_toolkit(i: &Value) -> Option<Toolkit> {
    let slug = i.get("slug")?.as_str()?.to_owned();
    if !crate::surface::toolkit_allowed(&slug)
        || i.get("kind").is_some_and(|v| v != "toolkit")
        || i.get("type")
            .is_some_and(|v| v != "toolkit" && v != "native")
        || i.get("toolkit_type").is_some_and(|v| v != "api")
        || i.get("is_local_toolkit").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let categories = i
        .pointer("/meta/categories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    c.as_str()
                        .or_else(|| c.get("name").and_then(Value::as_str))
                        .or_else(|| c.get("slug").and_then(Value::as_str))
                        .map(|s| crate::schema::clean_text(s, 40))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Toolkit {
        name: crate::schema::clean_text(i.get("name").and_then(Value::as_str).unwrap_or(&slug), 80),
        slug,
        description: crate::schema::clean_text(
            i.pointer("/meta/description")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            240,
        ),
        categories,
        tools_count: i.pointer("/meta/tools_count").and_then(Value::as_u64),
        logo: i
            .pointer("/meta/logo")
            .and_then(Value::as_str)
            .filter(|u| {
                u.len() <= 300
                    && u.starts_with("https://")
                    && !u.chars().any(|c| c.is_control() || c.is_whitespace())
            })
            .map(str::to_owned),
    })
}

fn parse_tool(i: &Value) -> Option<RemoteTool> {
    let slug = i.get("slug")?.as_str()?.to_owned();
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let toolkit = i.pointer("/toolkit/slug")?.as_str()?;
    if !crate::surface::action_allowed(toolkit, &slug)
        || ["kind", "type"]
            .iter()
            .any(|key| i.get(key).is_some_and(|v| v != "action"))
    {
        return None;
    }
    let version = i.get("version")?.as_str()?;
    if version.len() != 11
        || !version.as_bytes().iter().enumerate().all(|(n, b)| {
            if n == 8 {
                *b == b'_'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    let schema = crate::schema::sanitize(i.get("input_parameters")?);
    if !schema.is_object() {
        return None;
    }
    Some(RemoteTool {
        slug,
        toolkit: toolkit.to_owned(),
        version: version.to_owned(),
        description: crate::schema::clean_text(
            i.get("description")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            300,
        ),
        schema,
    })
}

/// Body of `POST /tools/execute/{slug}`.
///
/// Explicit account and concrete version fields verified against the local
/// TypeScript SDK `Tools.ts::executeComposioTool`. Live behavior still needs a spike.
fn execute_body(user_id: &str, version: &str, account_id: &str, arguments: &Value) -> Value {
    json!({"user_id": user_id, "connected_account_id": account_id, "version": version, "arguments": arguments})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_composio_https_hosts_are_accepted() {
        assert!(is_composio_https_url("https://connect.composio.dev/link/x"));
        assert!(is_composio_https_url(
            "https://app.composio.dev/link/lt_example"
        ));
        assert!(!is_composio_https_url("https://composio.dev/a"));
        for bad in [
            "https://app.composio.dev/",
            "https://app.composio.dev/link/../settings",
            "https://app.composio.dev/link/lt_example/other",
            "http://connect.composio.dev/x",
            "http://169.254.169.254/latest",
            "https://evilcomposio.dev/x",
            "https://composio.dev.evil.com/x",
            "https://user:pw@connect.composio.dev/x",
            "https://connect.composio.dev:8443/x",
            "https://logos.composio.dev/consent",
            "https://unexpected.composio.dev/consent",
            "https://%63onnect.composio.dev/x",
            "https://connect.composio.dev:443/x",
            "https://xn--composo-9za.dev/x",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!is_composio_https_url(bad), "{bad}");
        }
    }

    #[test]
    fn production_client_cannot_be_redirected_to_another_host() {
        let r: Redact = Arc::new(str::to_owned);
        assert!(
            ComposioClient::for_loopback_tests("k", "https://example.com/api", r.clone()).is_err()
        );
        assert!(ComposioClient::for_loopback_tests("k", "http://10.0.0.5/api", r.clone()).is_err());
        assert!(ComposioClient::for_loopback_tests("k", "http://127.0.0.1:1/api", r).is_ok());
    }
}
