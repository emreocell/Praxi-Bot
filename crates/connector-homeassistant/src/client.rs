//! Home Assistant REST client (`/api/`, `/api/states`, `/api/services`).
//!
//! Every request goes to the one address the owner gave, and nowhere else: the path is built
//! from validated names, redirects are refused, and the connection itself is checked against
//! a [`web_tools::UrlPolicy`] (see [`hub_policy`]) at the moment of connecting.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use secret_store::{SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::{Host, Url};
use web_tools::UrlPolicy;
use web_tools::policy::{DefaultUrlPolicy, is_loopback, is_private};

/// Largest response body read from the hub.
const MAX_BODY: usize = 4 << 20;

/// Services that run arbitrary code or stop the hub: never callable by a bot.
const DENIED: &[(&str, &str)] = &[
    ("shell_command", "*"),
    ("python_script", "*"),
    ("pyscript", "*"),
    ("hassio", "*"),
    ("rest_command", "*"),
    ("homeassistant", "restart"),
    ("homeassistant", "stop"),
    ("homeassistant", "reload_all"),
    ("recorder", "purge"),
    ("recorder", "purge_entities"),
];

/// Domains of plain devices: their services follow the bot's access level like any other write
/// (on Full, no card). Every OTHER domain asks the owner on every call, at every level, unless
/// the owner listed the exact service in `trusted_services`: `lock`, `alarm_control_panel` and
/// `cover` open, unlock or disarm things; `script`, `automation` and `scene` run other actions
/// that could; `button`, `input_*`, `siren`, `valve` and the raw protocols (`mqtt`, `zwave_js`,
/// `zha`, `esphome`, `knx` ...) can reach a lock or a garage door without naming one. A list of
/// what is safe is used instead of a list of what is not, so a domain nobody thought of asks.
const PLAIN_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "climate",
    "media_player",
    "vacuum",
    "humidifier",
    "water_heater",
    "number",
    "select",
    "notify",
    "tts",
];

/// Plain domains whose services may also be aimed by area, device, floor, label or `all`
/// (`light.turn_off` in the kitchen). A `switch` may not: it can be a garage door.
const AREA_DOMAINS: &[&str] = &[
    "light",
    "fan",
    "climate",
    "media_player",
    "vacuum",
    "humidifier",
    "water_heater",
    "notify",
    "tts",
];

/// A service whose name contains one of these asks, whatever its domain (`valve.open_valve`).
const ASK_SERVICE_WORDS: &[&str] = &["unlock", "disarm", "open"];

/// An entity whose name contains one of these asks: a garage door is often a plain `switch`.
const ASK_ENTITY_WORDS: &[&str] = &["garage", "gate", "door", "lock", "alarm"];

/// How to reach the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantConfig {
    /// Base URL, e.g. `http://homeassistant.local:8123/`.
    pub url: String,
    /// Secret-store reference of the long-lived access token.
    pub token: SecretRef,
    /// When set, only these domains (`light`, `switch` ...) may be called.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Per-request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Services (`cover.open_cover`) the owner lets bots call without a card even though they
    /// would ask (see [`HomeAssistantClient::needs_confirmation`]); a listed service runs
    /// without a card on every entity. Exact `domain.service` names; `homeassistant.*` can
    /// never be listed.
    #[serde(default)]
    pub trusted_services: Vec<String>,
    /// The bots that may use Home Assistant: absent = every bot, an empty list = none.
    /// Bot storage ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bots: Option<Vec<String>>,
}

fn default_timeout() -> u64 {
    15
}

/// Why a call failed. Never contains the token.
#[derive(Debug, thiserror::Error)]
pub enum HaError {
    /// Bad configuration or arguments.
    #[error("{0}")]
    Invalid(String),
    /// The secret could not be read.
    #[error("cannot read the Home Assistant token from the secret store: {0}")]
    Credential(String),
    /// Network or HTTP failure.
    #[error("home assistant: {0}")]
    Server(String),
}

/// A configured hub.
pub struct HomeAssistantClient {
    base: Url,
    token: SecretRef,
    allowed: Vec<String>,
    trusted: Vec<String>,
    store: Arc<dyn SecretStore>,
    http: reqwest::Client,
}

/// What a connection test found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// How many entities the hub reports.
    pub entities: usize,
}

/// The policy for the hub address: LAN and loopback stay open on purpose (Home Assistant runs
/// on the home network), but link-local and cloud-metadata space, the unspecified and
/// multicast ranges and the daemon's own API socket (`protected`) are never dialled, whatever
/// name leads there.
#[must_use]
pub fn hub_policy(protected: &[SocketAddr]) -> Arc<dyn UrlPolicy> {
    Arc::new(DefaultUrlPolicy {
        block_private: false,
        block_loopback: false,
        blocked_sockets: protected.to_vec(),
        ..DefaultUrlPolicy::default()
    })
}

/// Resolves the hub's name and drops every address the policy refuses, when connecting (so a
/// name that later points elsewhere is caught). A plain-`http` hub is only reached on a LAN or
/// loopback address: the token never crosses the internet in clear.
struct GuardedResolver {
    policy: Arc<dyn UrlPolicy>,
    port: u16,
    plain_http: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let (policy, port, plain) = (Arc::clone(&self.policy), self.port, self.plain_http);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            let allowed: Vec<SocketAddr> = all
                .into_iter()
                .filter(|a| !plain || is_loopback(a.ip()) || is_private(a.ip()))
                .filter(|a| policy.check_socket(&host, *a).is_ok())
                .collect();
            if allowed.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    "the address leads somewhere Home Assistant may not be",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

fn is_private_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => {
            let ip = IpAddr::V4(ip);
            is_loopback(ip) || is_private(ip)
        }
        Some(Host::Ipv6(ip)) => {
            let ip = IpAddr::V6(ip);
            is_loopback(ip) || is_private(ip)
        }
        Some(Host::Domain(d)) => {
            d == "localhost" || d.rsplit('.').next() == Some("local") || !d.contains('.')
        }
        None => false,
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `domain.object`, lowercase ASCII only (what the hub itself accepts after normalising).
fn valid_entity(id: &str) -> bool {
    id.split_once('.')
        .is_some_and(|(d, o)| valid_name(d) && valid_name(o))
}

/// What a service call is aimed at: the entity ids named by `entity_id` and `data.entity_id`
/// (each a string, possibly comma separated, or a list).
struct Targets {
    ids: Vec<String>,
    /// Every id is a plain `domain.object`, and at least one is named.
    explicit: bool,
    /// Something that is not an id, `all` or `none` (odd characters a hub may normalise into an
    /// id this code did not see, or the wrong JSON type).
    malformed: bool,
}

fn call_targets(input: &Value) -> Targets {
    let mut t = Targets {
        ids: Vec::new(),
        explicit: true,
        malformed: false,
    };
    let mut take = |s: &str| {
        let id = s.trim().to_owned();
        if id.is_empty() {
            return;
        }
        if id == "all" || id == "none" {
            t.explicit = false;
            return;
        }
        if !valid_entity(&id) {
            t.malformed = true;
        }
        t.ids.push(id);
    };
    for v in [&input["entity_id"], &input["data"]["entity_id"]] {
        match v {
            Value::String(s) => {
                for part in s.split(',') {
                    take(part);
                }
            }
            Value::Array(a) => {
                for e in a {
                    take(e.as_str().unwrap_or("?"));
                }
            }
            Value::Null => {}
            _ => take("?"),
        }
    }
    for key in ["area_id", "device_id", "floor_id", "label_id"] {
        if !input["data"][key].is_null() {
            t.explicit = false;
        }
    }
    if t.ids.is_empty() {
        t.explicit = false;
    }
    t
}

/// An entity that makes a call ask: not a plain device, or a name that hints at a door.
fn entity_asks(id: &str) -> bool {
    let (domain, object) = id.split_once('.').unwrap_or((id, ""));
    !PLAIN_DOMAINS.contains(&domain)
        && !matches!(
            domain,
            "sensor" | "binary_sensor" | "weather" | "sun" | "person" | "zone"
        )
        || ASK_ENTITY_WORDS.iter().any(|w| object.contains(w))
}

/// `entity_id` of a service call, checked before it is sent: ids (comma separated is fine),
/// `all` or `none`, nothing a hub could read differently from this code.
fn check_entity_ids(v: &Value, what: &str) -> Result<(), HaError> {
    let bad = || HaError::Invalid(format!("`{what}` must be entity ids like light.kitchen"));
    let one = |s: &str| {
        s.split(',')
            .map(str::trim)
            .all(|p| valid_entity(p) || p == "all" || p == "none")
    };
    match v {
        Value::Null => Ok(()),
        Value::String(s) if one(s) => Ok(()),
        Value::Array(a) if a.iter().all(|e| e.as_str().is_some_and(&one)) => Ok(()),
        _ => Err(bad()),
    }
}

impl HomeAssistantClient {
    /// A client under the default [`hub_policy`].
    ///
    /// # Errors
    /// [`HaError::Invalid`] for a bad URL or plain HTTP to a public host.
    pub fn new(cfg: HomeAssistantConfig, store: Arc<dyn SecretStore>) -> Result<Self, HaError> {
        Self::with_policy(cfg, store, &hub_policy(&[]))
    }

    /// A client that only dials what `policy` allows.
    ///
    /// # Errors
    /// [`HaError::Invalid`] for a bad URL, plain HTTP to a public host, or an address the
    /// policy refuses.
    pub fn with_policy(
        cfg: HomeAssistantConfig,
        store: Arc<dyn SecretStore>,
        policy: &Arc<dyn UrlPolicy>,
    ) -> Result<Self, HaError> {
        let mut base =
            Url::parse(&cfg.url).map_err(|e| HaError::Invalid(format!("bad url: {e}")))?;
        match base.scheme() {
            "https" => {}
            "http" if is_private_host(&base) => {}
            _ => {
                return Err(HaError::Invalid(
                    "the url must be https (plain http only for a home-network or localhost address)"
                        .into(),
                ));
            }
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(HaError::Invalid("do not put credentials in the url".into()));
        }
        policy
            .check_url(&base)
            .map_err(|e| HaError::Invalid(format!("this address is not allowed: {e}")))?;
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // A system or environment proxy would see the token and skip the address check.
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver {
                policy: Arc::clone(policy),
                port: base.port_or_known_default().unwrap_or(80),
                plain_http: base.scheme() == "http",
            }))
            .timeout(Duration::from_secs(cfg.timeout_secs.clamp(1, 60)))
            .build()
            .map_err(|e| HaError::Server(e.to_string()))?;
        Ok(Self {
            base,
            token: cfg.token,
            allowed: cfg.allowed_domains,
            trusted: cfg
                .trusted_services
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.starts_with("homeassistant."))
                .collect(),
            store,
            http,
        })
    }

    async fn token(&self) -> Result<String, HaError> {
        let (store, r) = (Arc::clone(&self.store), self.token.clone());
        tokio::task::spawn_blocking(move || {
            resolve(store.as_ref(), &r).map(|v| v.expose().to_string())
        })
        .await
        .map_err(|e| HaError::Credential(e.to_string()))?
        .map_err(|e| HaError::Credential(e.to_string()))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, HaError> {
        let url = self
            .base
            .join(path)
            .map_err(|e| HaError::Invalid(e.to_string()))?;
        let mut req = self
            .http
            .request(method, url)
            .bearer_auth(self.token().await?);
        if let Some(b) = body {
            req = req.json(b);
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| HaError::Server(scrub(&e.without_url().to_string())))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(HaError::Server(format!(
                "the hub answered with a redirect ({status}); use its final address"
            )));
        }
        // Read in chunks and stop at the limit: a hub cannot make this buffer gigabytes.
        let too_large = || HaError::Server("response too large".into());
        if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
            return Err(too_large());
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| HaError::Server(scrub(&e.without_url().to_string())))?
        {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(HaError::Server(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    format!("the hub refused the access token ({status})")
                }
                StatusCode::NOT_FOUND => "not found (unknown entity or service)".to_owned(),
                _ => format!("HTTP {status}"),
            }));
        }
        serde_json::from_slice(&bytes).map_err(|e| HaError::Server(format!("bad JSON: {e}")))
    }

    /// Check the address and token the way a person would: `GET /api/`, then count the entities.
    ///
    /// # Errors
    /// See [`HaError`].
    pub async fn probe(&self) -> Result<Probe, HaError> {
        self.request(reqwest::Method::GET, "api/", None).await?;
        let states = self
            .request(reqwest::Method::GET, "api/states", None)
            .await?;
        let entities = states
            .as_array()
            .ok_or_else(|| HaError::Server("this does not look like Home Assistant".into()))?
            .len();
        Ok(Probe { entities })
    }

    /// Whether this service call must ask the owner on every call, at every level (Full
    /// included). It asks unless it is a plain device service (see `PLAIN_DOMAINS`: lights,
    /// switches, climate, media ...) AND its name has none of `unlock`, `disarm`, `open` AND
    /// every entity it names is a plain device without a door-like name (`garage`, `gate`,
    /// `door`, `lock`, `alarm`) AND, outside the area-friendly domains (`AREA_DOMAINS`), it
    /// names its entities (no area, device, label, `all`, or nothing: those reach things this
    /// code cannot see). The generic `homeassistant.*` follows the same rule. The owner lists
    /// `domain.service` names in `trusted_services` to stop the card for one of them, on every
    /// entity. A call whose domain or service is not stated clearly, or whose entity ids are
    /// odd, asks.
    #[must_use]
    pub fn needs_confirmation(&self, input: &Value) -> bool {
        let name = |k: &str| input[k].as_str().map(|s| s.trim().to_ascii_lowercase());
        let (Some(domain), Some(service)) = (name("domain"), name("service")) else {
            return true;
        };
        if domain != "homeassistant" && self.trusted.contains(&format!("{domain}.{service}")) {
            return false;
        }
        let t = call_targets(input);
        let plain = PLAIN_DOMAINS.contains(&domain.as_str()) || domain == "homeassistant";
        !plain
            || ASK_SERVICE_WORDS.iter().any(|w| service.contains(w))
            || t.malformed
            || t.ids.iter().any(|i| entity_asks(i))
            || (!t.explicit && !AREA_DOMAINS.contains(&domain.as_str()))
    }

    /// Entities, optionally of one domain, as `{entity_id, state, name}`.
    ///
    /// # Errors
    /// See [`HaError`].
    pub async fn list(&self, domain: Option<&str>, limit: usize) -> Result<Vec<Value>, HaError> {
        if let Some(d) = domain
            && !valid_name(d)
        {
            return Err(HaError::Invalid(format!("`{d}` is not a domain name")));
        }
        let all = self
            .request(reqwest::Method::GET, "api/states", None)
            .await?;
        let prefix = domain.map(|d| format!("{d}."));
        Ok(all
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| {
                let id = s["entity_id"].as_str().unwrap_or_default();
                prefix.as_deref().is_none_or(|p| id.starts_with(p))
            })
            .take(limit)
            .map(|s| {
                serde_json::json!({
                    "entity_id": s["entity_id"],
                    "state": s["state"],
                    "name": s["attributes"]["friendly_name"],
                })
            })
            .collect())
    }

    /// One entity with its attributes.
    ///
    /// # Errors
    /// See [`HaError`].
    pub async fn state(&self, entity_id: &str) -> Result<Value, HaError> {
        let ok = entity_id
            .split_once('.')
            .is_some_and(|(d, o)| valid_name(d) && valid_name(o));
        if !ok {
            return Err(HaError::Invalid(format!(
                "`{entity_id}` is not an entity id (domain.object)"
            )));
        }
        self.request(
            reqwest::Method::GET,
            &format!("api/states/{entity_id}"),
            None,
        )
        .await
    }

    /// Call `domain.service` with optional target and data; returns the
    /// entities whose state changed.
    ///
    /// # Errors
    /// [`HaError::Invalid`] for a denied or not allowed service.
    pub async fn call(
        &self,
        domain: &str,
        service: &str,
        entity_id: Option<&str>,
        data: Option<&Value>,
    ) -> Result<Value, HaError> {
        if !valid_name(domain) || !valid_name(service) {
            return Err(HaError::Invalid(
                "domain and service are lowercase names".into(),
            ));
        }
        if DENIED
            .iter()
            .any(|(d, s)| *d == domain && (*s == "*" || *s == service))
        {
            return Err(HaError::Invalid(format!(
                "`{domain}.{service}` is blocked: it can run code or stop the hub"
            )));
        }
        if !self.allowed.is_empty() && !self.allowed.iter().any(|d| d == domain) {
            return Err(HaError::Invalid(format!(
                "domain `{domain}` is not in `allowed_domains`"
            )));
        }
        let mut body = match data {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            Some(_) => return Err(HaError::Invalid("`data` must be an object".into())),
            None => serde_json::json!({}),
        };
        check_entity_ids(&body["entity_id"], "data.entity_id")?;
        if let Some(e) = entity_id {
            check_entity_ids(&Value::String(e.to_owned()), "entity_id")?;
            body["entity_id"] = Value::String(e.to_owned());
        }
        self.request(
            reqwest::Method::POST,
            &format!("api/services/{domain}/{service}"),
            Some(&body),
        )
        .await
    }
}

/// Drop anything that looks like a credential-bearing URL from a transport error.
fn scrub(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            if w.contains("://") && w.contains('@') {
                "<url>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
