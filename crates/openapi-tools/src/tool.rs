//! Tool construction and the HTTP executor.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{ACCEPT, HeaderName, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, Url, redirect::Policy};
use secret_store::{Redactor, SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError,
    ToolOutput, ToolRegistry, ToolSpec,
};

use crate::error::OpenApiError;
use crate::import::{
    ApiSpec, BodyKind, HttpMethod, ImportOptions, Operation, ParamBinding, ParamLocation,
};

/// Credentials for an API, expressed as secret *references*. The values are
/// resolved from the [`SecretStore`] at call time and are never part of a tool
/// schema, prompt, error message or returned body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// No credentials.
    None,
    /// API key sent in a request header.
    ApiKeyHeader {
        /// Header name, e.g. `X-Api-Key`.
        header: String,
        /// Secret holding the key.
        secret: SecretRef,
    },
    /// API key sent as a query parameter.
    ApiKeyQuery {
        /// Query parameter name.
        param: String,
        /// Secret holding the key.
        secret: SecretRef,
    },
    /// `Authorization: Bearer <secret>`.
    Bearer {
        /// Secret holding the token.
        secret: SecretRef,
    },
    /// HTTP basic auth.
    Basic {
        /// User name (not secret).
        username: String,
        /// Secret holding the password.
        password_secret: SecretRef,
    },
}

impl Auth {
    fn secret_refs(&self) -> Vec<&SecretRef> {
        match self {
            Self::None => vec![],
            Self::ApiKeyHeader { secret, .. }
            | Self::ApiKeyQuery { secret, .. }
            | Self::Bearer { secret } => vec![secret],
            Self::Basic {
                password_secret, ..
            } => vec![password_secret],
        }
    }
}

/// Runtime settings shared by all tools of one API.
#[derive(Clone)]
pub struct OpenApiConfig {
    /// Where secret references are resolved.
    pub secrets: Arc<dyn SecretStore>,
    /// Credentials.
    pub auth: Auth,
    /// Total request timeout.
    pub timeout: Duration,
    /// Maximum response bytes kept (further capped by
    /// [`ToolContext::max_inline_bytes`]).
    pub max_response_bytes: usize,
    /// Allow loopback / private-network hosts (SSRF guard off).
    pub allow_private_hosts: bool,
    /// Permission category of every generated tool.
    pub category: PermissionCategory,
    /// `User-Agent` header.
    pub user_agent: String,
}

impl OpenApiConfig {
    /// Defaults: no auth, 30 s timeout, 256 KiB responses, private hosts
    /// blocked, [`PermissionCategory::Accounts`].
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets,
            auth: Auth::None,
            timeout: Duration::from_secs(30),
            max_response_bytes: 256 * 1024,
            allow_private_hosts: false,
            category: PermissionCategory::Accounts,
            user_agent: concat!("praxi-bot-openapi-tools/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

/// Effect classification: GET/HEAD/OPTIONS read, DELETE destructive, the rest write.
#[must_use]
pub fn effect_for(method: HttpMethod) -> Effect {
    match method {
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options => Effect::Read,
        HttpMethod::Delete => Effect::Destructive,
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch => Effect::Write,
    }
}

/// Build one [`Tool`] per operation of `spec`.
///
/// # Errors
/// [`OpenApiError::PrivateHost`] when the base URL points at a private host
/// and `cfg.allow_private_hosts` is false; [`OpenApiError::Config`] for invalid
/// auth settings.
///
/// # SSRF guard
/// With `allow_private_hosts == false` the base URL's literal host is checked
/// here and every DNS answer is filtered again at connect time, so names such
/// as `127.0.0.1.nip.io` or rebinding hosts cannot reach private ranges.
pub fn tools(spec: &ApiSpec, cfg: &OpenApiConfig) -> Result<Vec<Arc<dyn Tool>>, OpenApiError> {
    let base = Url::parse(&spec.base_url).map_err(|e| OpenApiError::NoServer(e.to_string()))?;
    if !cfg.allow_private_hosts
        && let Some(host) = private_host(&base)
    {
        return Err(OpenApiError::PrivateHost(host));
    }
    for r in cfg.auth.secret_refs() {
        secret_store::validate_name(r.name())
            .map_err(|e| OpenApiError::Config(format!("secret reference: {e}")))?;
    }
    match &cfg.auth {
        Auth::ApiKeyHeader { header, .. } => {
            HeaderName::from_bytes(header.as_bytes())
                .map_err(|_| OpenApiError::Config(format!("invalid header name `{header}`")))?;
        }
        Auth::ApiKeyQuery { param, .. } if param.is_empty() => {
            return Err(OpenApiError::Config("empty query parameter name".into()));
        }
        _ => {}
    }
    let mut builder = Client::builder()
        // Ambient proxies can resolve the destination themselves, bypassing
        // GuardResolver and receiving credentials intended only for that API.
        .no_proxy()
        .timeout(cfg.timeout)
        .connect_timeout(cfg.timeout.min(Duration::from_secs(10)))
        .redirect(Policy::none())
        .user_agent(cfg.user_agent.clone());
    if !cfg.allow_private_hosts {
        // Re-checks every resolved address at connect time (DNS names that point
        // at private ranges, DNS rebinding).
        builder = builder.dns_resolver(Arc::new(GuardResolver));
    }
    let client = builder
        .build()
        .map_err(|e| OpenApiError::Config(format!("http client: {e}")))?;
    let shared = Arc::new(Shared {
        base_url: spec.base_url.clone(),
        client,
        cfg: cfg.clone(),
    });
    Ok(spec
        .operations
        .iter()
        .map(|op| {
            let mut op = op.clone();
            // Credentials are injected by the executor; never expose (or let the
            // model override) the parameter that carries them.
            op.params.retain(|p| !shared.is_auth_param(p));
            Arc::new(OpenApiTool {
                spec: tool_spec(&op, spec, cfg.category),
                op,
                shared: shared.clone(),
            }) as Arc<dyn Tool>
        })
        .collect())
}

/// Register every tool of `spec` in `registry`. Returns how many were added
/// (name collisions with existing tools are skipped).
///
/// # Errors
/// See [`tools`].
pub fn register(
    registry: &ToolRegistry,
    spec: &ApiSpec,
    cfg: &OpenApiConfig,
) -> Result<usize, OpenApiError> {
    Ok(tools(spec, cfg)?
        .into_iter()
        .filter(|t| registry.register(t.clone()))
        .count())
}

/// Parse `text` and build its tools in one step.
///
/// # Errors
/// See [`ApiSpec::parse`] and [`tools`].
pub fn import_tools(
    text: &str,
    opts: &ImportOptions,
    cfg: &OpenApiConfig,
) -> Result<(ApiSpec, Vec<Arc<dyn Tool>>), OpenApiError> {
    let spec = ApiSpec::parse(text, opts)?;
    let t = tools(&spec, cfg)?;
    Ok((spec, t))
}

fn tool_spec(op: &Operation, api: &ApiSpec, category: PermissionCategory) -> ToolSpec {
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in &op.params {
        props.insert(p.key.clone(), p.schema.clone());
        if p.required {
            required.push(json!(p.key));
        }
    }
    if let Some(b) = &op.body {
        props.insert("body".into(), b.schema.clone());
        if b.required {
            required.push(json!("body"));
        }
    }
    let title = if api.title.is_empty() {
        String::new()
    } else {
        format!("({}) ", api.title)
    };
    ToolSpec {
        name: op.name.clone(),
        description: format!("{title}{}", op.description),
        input_schema: json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        }),
        effect: effect_for(op.method),
        category,
        platforms: vec![Platform::All],
        // Every operation contacts the API host (egress) and the response can
        // be both private account data and untrusted third-party text.
        content: ContentClass::all().with_external_metadata(),
    }
}

struct Shared {
    base_url: String,
    client: Client,
    cfg: OpenApiConfig,
}

impl Shared {
    fn is_auth_param(&self, p: &ParamBinding) -> bool {
        match &self.cfg.auth {
            Auth::ApiKeyHeader { header, .. } => {
                p.location == ParamLocation::Header && p.name.eq_ignore_ascii_case(header)
            }
            Auth::ApiKeyQuery { param, .. } => {
                p.location == ParamLocation::Query && p.name == *param
            }
            _ => false,
        }
    }

    fn secret(&self, r: &SecretRef, redactor: &mut Redactor) -> Result<String, ToolError> {
        let v = resolve(self.cfg.secrets.as_ref(), r)
            .map_err(|e| ToolError::Execution(format!("credential unavailable: {e}")))?;
        redactor.add_secret(v.expose());
        Ok(v.expose().to_string())
    }

    /// Attach credentials; registers every secret (and derived form) with `redactor`.
    fn apply_auth(
        &self,
        mut rb: RequestBuilder,
        redactor: &mut Redactor,
    ) -> Result<RequestBuilder, ToolError> {
        match &self.cfg.auth {
            Auth::None => {}
            Auth::ApiKeyHeader { header, secret } => {
                let key = self.secret(secret, redactor)?;
                let mut v = HeaderValue::from_str(&key).map_err(|_| {
                    ToolError::Execution("credential is not a valid header value".into())
                })?;
                v.set_sensitive(true);
                rb = rb.header(header.as_str(), v);
            }
            Auth::ApiKeyQuery { param, secret } => {
                let key = self.secret(secret, redactor)?;
                // Also mask the percent-encoded form in case the server echoes the URL.
                redactor.add_secret(&encode_component(&key));
                rb = rb.query(&[(param.as_str(), key.as_str())]);
            }
            Auth::Bearer { secret } => {
                let token = self.secret(secret, redactor)?;
                let mut v = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                    ToolError::Execution("credential is not a valid header value".into())
                })?;
                v.set_sensitive(true);
                rb = rb.header(reqwest::header::AUTHORIZATION, v);
            }
            Auth::Basic {
                username,
                password_secret,
            } => {
                let pw = self.secret(password_secret, redactor)?;
                rb = rb.basic_auth(username, Some(&pw));
                redactor.add_secret(&base64_basic(username, &pw));
            }
        }
        Ok(rb)
    }
}

struct OpenApiTool {
    spec: ToolSpec,
    op: Operation,
    shared: Arc<Shared>,
}

fn method_of(m: HttpMethod) -> Method {
    match m {
        HttpMethod::Get => Method::GET,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Options => Method::OPTIONS,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
    }
}

/// Everything except RFC 3986 unreserved characters.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn encode_component(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

fn base64_basic(user: &str, pw: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"))
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        other => other.to_string(),
    }
}

/// Query-style serialization (`form`).
fn push_query(p: &ParamBinding, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Array(a) if p.explode => {
            out.extend(a.iter().map(|x| (p.name.clone(), scalar(x))));
        }
        Value::Array(a) => out.push((
            p.name.clone(),
            a.iter().map(scalar).collect::<Vec<_>>().join(","),
        )),
        Value::Object(o) if p.explode => {
            out.extend(o.iter().map(|(k, x)| (k.clone(), scalar(x))));
        }
        Value::Object(o) => out.push((
            p.name.clone(),
            o.iter()
                .flat_map(|(k, x)| [k.clone(), scalar(x)])
                .collect::<Vec<_>>()
                .join(","),
        )),
        _ => out.push((p.name.clone(), scalar(v))),
    }
}

/// Path / header style serialization (`simple`).
fn simple(p: &ParamBinding, v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().map(scalar).collect::<Vec<_>>().join(","),
        Value::Object(o) if p.explode => o
            .iter()
            .map(|(k, x)| format!("{k}={}", scalar(x)))
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(o) => o
            .iter()
            .flat_map(|(k, x)| [k.clone(), scalar(x)])
            .collect::<Vec<_>>()
            .join(","),
        _ => scalar(v),
    }
}

fn invalid(msg: impl Into<String>) -> ToolError {
    ToolError::InvalidInput(msg.into())
}

impl OpenApiTool {
    /// Validate `input` and build the request (without credentials).
    fn build(&self, input: Value) -> Result<RequestBuilder, ToolError> {
        let obj = match input {
            Value::Object(o) => o,
            Value::Null => Map::new(),
            _ => return Err(invalid("input must be a JSON object")),
        };
        let known: HashSet<&str> = self
            .op
            .params
            .iter()
            .map(|p| p.key.as_str())
            .chain(self.op.body.iter().map(|_| "body"))
            .collect();
        let mut unknown: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|k| !known.contains(k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(invalid(format!(
                "unknown parameter(s): {}",
                unknown.join(", ")
            )));
        }
        let get = |p: &ParamBinding| obj.get(&p.key).filter(|v| !v.is_null());
        for p in &self.op.params {
            if p.required && get(p).is_none() {
                return Err(invalid(format!("missing required parameter `{}`", p.key)));
            }
        }
        if self.op.body.as_ref().is_some_and(|b| b.required)
            && obj.get("body").is_none_or(Value::is_null)
        {
            return Err(invalid("missing required parameter `body`"));
        }

        // Path.
        let mut path = String::new();
        let mut rest = self.op.path.as_str();
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                break;
            };
            let name = &rest[start + 1..start + len];
            let p = self
                .op
                .params
                .iter()
                .find(|p| p.location == ParamLocation::Path && p.name == name)
                .ok_or_else(|| invalid(format!("no binding for path parameter `{name}`")))?;
            let v =
                get(p).ok_or_else(|| invalid(format!("missing required parameter `{}`", p.key)))?;
            path.push_str(&rest[..start]);
            let raw = simple(p, v);
            // `Url::parse` collapses dot segments, which would let the model
            // reach other endpoints (or leave the base path).
            if raw.to_ascii_lowercase().replace("%2e", ".").trim() == "."
                || raw.to_ascii_lowercase().replace("%2e", ".").trim() == ".."
            {
                return Err(invalid(format!(
                    "path parameter `{}` must not be `.` or `..`",
                    p.key
                )));
            }
            path.push_str(&encode_component(&raw));
            rest = &rest[start + len + 1..];
        }
        path.push_str(rest);
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        let url = format!("{}{path}", self.shared.base_url);
        let url = Url::parse(&url).map_err(|e| invalid(format!("invalid request URL: {e}")))?;

        let mut rb = self
            .shared
            .client
            .request(method_of(self.op.method), url)
            .header(ACCEPT, "application/json, text/plain;q=0.8, */*;q=0.5");

        // Query & headers.
        let mut query = Vec::new();
        for p in &self.op.params {
            let Some(v) = get(p) else { continue };
            match p.location {
                ParamLocation::Path => {}
                ParamLocation::Query => push_query(p, v, &mut query),
                ParamLocation::Header => {
                    let name = HeaderName::from_bytes(p.name.as_bytes())
                        .map_err(|_| invalid(format!("invalid header name `{}`", p.name)))?;
                    let value = HeaderValue::from_str(&simple(p, v))
                        .map_err(|_| invalid(format!("invalid value for header `{}`", p.name)))?;
                    rb = rb.header(name, value);
                }
            }
        }
        if let Auth::ApiKeyQuery { param, .. } = &self.shared.cfg.auth {
            // Exploded objects can carry arbitrary keys; never let input
            // supply or shadow the credential parameter.
            query.retain(|(k, _)| k != param);
        }
        if !query.is_empty() {
            rb = rb.query(&query);
        }

        // Body.
        if let (Some(b), Some(v)) = (&self.op.body, obj.get("body").filter(|v| !v.is_null())) {
            match b.kind {
                BodyKind::Json => {
                    let bytes = serde_json::to_vec(v).map_err(|e| invalid(e.to_string()))?;
                    rb = rb
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(bytes);
                }
                BodyKind::Form => {
                    let Value::Object(o) = v else {
                        return Err(invalid("`body` must be an object for form encoding"));
                    };
                    let mut pairs = Vec::new();
                    for (k, x) in o {
                        match x {
                            Value::Array(a) => {
                                pairs.extend(a.iter().map(|i| (k.clone(), scalar(i))));
                            }
                            _ => pairs.push((k.clone(), scalar(x))),
                        }
                    }
                    rb = rb.form(&pairs);
                }
            }
        }
        Ok(rb)
    }
}

fn is_texty(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.is_empty()
        || ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("yaml")
        || ct.contains("javascript")
        || ct.contains("x-www-form-urlencoded")
}

#[async_trait]
impl Tool for OpenApiTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let rb = self.build(input)?;
        let mut redactor = Redactor::new();
        let rb = self.shared.apply_auth(rb, &mut redactor)?;
        let cap = self
            .shared
            .cfg
            .max_response_bytes
            .min(ctx.max_inline_bytes)
            .max(1);

        let mut resp = rb.send().await.map_err(|e| {
            let msg = if e.is_timeout() {
                format!(
                    "request timed out after {}s",
                    self.shared.cfg.timeout.as_secs()
                )
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                redactor.redact(&e.without_url().to_string())
            };
            ToolError::Execution(msg)
        })?;

        let status = resp.status();
        let header = |n: &str| {
            resp.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(|s| redactor.redact(s))
        };
        let content_type = header("content-type").unwrap_or_default();
        let location = header("location");
        let retry_after = header("retry-after");

        let mut bytes: Vec<u8> = Vec::new();
        let mut truncated = false;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    let room = cap - bytes.len();
                    if chunk.len() > room {
                        bytes.extend_from_slice(&chunk[..room]);
                        truncated = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(ToolError::Execution(format!(
                        "failed reading response: {}",
                        redactor.redact(&e.without_url().to_string())
                    )));
                }
            }
        }

        let mut out = Map::new();
        out.insert("status".into(), json!(status.as_u16()));
        out.insert("ok".into(), json!(status.is_success()));
        if !content_type.is_empty() {
            out.insert("content_type".into(), json!(content_type));
        }
        if let Some(l) = location {
            out.insert("location".into(), json!(l));
        }
        if let Some(r) = retry_after {
            out.insert("retry_after".into(), json!(r));
        }
        out.insert("truncated".into(), json!(truncated));

        let text =
            decode_text(&bytes, truncated).filter(|_| !bytes.is_empty() && is_texty(&content_type));
        let body = match text {
            None if bytes.is_empty() => Value::Null,
            None => {
                out.insert(
                    "note".into(),
                    json!(format!(
                        "binary response ({} bytes{}) not shown",
                        bytes.len(),
                        if truncated { "+" } else { "" }
                    )),
                );
                Value::Null
            }
            Some(t) => {
                let t = redactor.redact(&t);
                let is_json = content_type.to_ascii_lowercase().contains("json");
                match (is_json && !truncated)
                    .then(|| serde_json::from_str::<Value>(&t).ok())
                    .flatten()
                {
                    Some(v) => v,
                    None => Value::String(t),
                }
            }
        };
        out.insert("body".into(), body);
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: Value::Object(out),
            }],
            spill: None,
            exposure: None,
        })
    }
}

/// Decode `bytes` as UTF-8. When the body was truncated, a multi-byte
/// character cut by the cap is dropped instead of making the text "binary".
fn decode_text(bytes: &[u8], truncated: bool) -> Option<String> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(s.to_string()),
        Err(e) if truncated && e.error_len().is_none() => {
            // Only an incomplete trailing sequence: back off to the boundary.
            Some(String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned())
        }
        Err(_) => None,
    }
}

/// DNS resolver that drops private / loopback / reserved addresses.
struct GuardResolver;

fn filter_public(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    addrs
        .into_iter()
        .filter(|a| !is_private_ip(a.ip()))
        .collect()
}

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let all = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let ok = filter_public(all);
            if ok.is_empty() {
                let e: Box<dyn std::error::Error + Send + Sync> =
                    format!("`{host}` resolves only to private or reserved addresses").into();
                return Err(e);
            }
            Ok(Box::new(ok.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => is_private_v4(v),
        IpAddr::V6(v) => is_private_v6(v),
    }
}

fn private_host(url: &Url) -> Option<String> {
    match url.host()? {
        url::Host::Domain(d) => {
            let d = d.to_ascii_lowercase();
            let d = d.trim_end_matches('.');
            let suffixed =
                |s: &str| d.len() > s.len() && d[d.len() - s.len()..].eq_ignore_ascii_case(s);
            (d == "localhost"
                || suffixed(".localhost")
                || suffixed(".local")
                || suffixed(".internal"))
            .then(|| d.to_string())
        }
        url::Host::Ipv4(ip) => is_private_v4(ip).then(|| ip.to_string()),
        url::Host::Ipv6(ip) => is_private_v6(ip).then(|| ip.to_string()),
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] >= 240
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (18..20).contains(&o[1]))
}

fn is_private_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_v4(v4);
    }
    let seg = ip.segments();
    let embedded = |hi: u16, lo: u16| {
        let (hi, lo) = (hi.to_be_bytes(), lo.to_be_bytes());
        is_private_v4(Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]))
    };
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // 6to4 (2002::/16) embeds the v4 address in bits 16..48.
        || (seg[0] == 0x2002 && embedded(seg[1], seg[2]))
        // NAT64 well-known prefix 64:ff9b::/96.
        || (seg[0] == 0x64 && seg[1] == 0xff9b && seg[2..6] == [0; 4] && embedded(seg[6], seg[7]))
        // IPv4-compatible ::/96.
        || (seg[..6] == [0; 6] && embedded(seg[6], seg[7]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_cut_in_the_middle_of_a_char_is_backed_off() {
        let b = "ğğğ".as_bytes();
        assert_eq!(decode_text(&b[..5], true).as_deref(), Some("ğğ"));
        assert_eq!(decode_text(&b[..5], false), None);
        assert_eq!(decode_text(&[0xff, 0xfe], true), None);
    }

    #[test]
    fn resolver_filter_drops_private_addresses() {
        let a = |s: &str| SocketAddr::new(s.parse().unwrap(), 0);
        let kept = filter_public([
            a("127.0.0.1"),
            a("169.254.169.254"),
            a("224.0.0.1"),
            a("240.0.0.1"),
            a("198.18.0.1"),
            a("192.0.0.8"),
            a("2002:7f00:1::1"),
            a("64:ff9b::a00:1"),
            a("::ffff:10.0.0.1"),
            a("ff02::1"),
            a("93.184.216.34"),
            a("2606:4700::1111"),
        ]);
        assert_eq!(kept, vec![a("93.184.216.34"), a("2606:4700::1111")]);
    }
}
