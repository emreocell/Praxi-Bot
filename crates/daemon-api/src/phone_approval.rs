//! Approving a money ask from the phone (ADR `docs/design/phone-money-approval-adr.md`).
//!
//! The owner turns it on with a PIN (stored as an Argon2id hash) and the public address of a
//! tunnel they choose. For a pending money ask, a holder of the owner key (the Telegram
//! channel) mints a one-time link: a 32-byte nonce, bound to that exact ask, valid 10 minutes.
//! The page behind it shows the facts, asks for the PIN, then shows the checkout screenshot and
//! Approve / Deny. What Telegram carries (the link) is never enough on its own.
//!
//! Routes: `GET|PUT|DELETE /v1/phone-approval` and `POST /v1/phone-approval/links` are owner
//! routes; `GET /v1/phone-approval/p/{nonce}`, `POST …/unlock` and `POST …/decide` are public
//! (no bearer: the nonce plus the PIN are the credential, see `auth::guard`). Peers never mint
//! or open the page (`peer.rs`).

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use agent_core::ApprovalScope;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use protocol::ToolCallId;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::dto::ApprovalDto;
use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

/// How long a phone link works after it was minted.
pub(crate) const LINK_TTL_MS: i64 = 10 * 60 * 1000;
/// Wrong PINs one link takes before it is burned.
const LINK_ATTEMPTS: u8 = 3;
/// Wrong PINs (across all links) within [`LOCK_WINDOW_MS`] that lock phone approval.
const LOCK_FAILURES: usize = 5;
/// The window of [`LOCK_FAILURES`], and how long the lock lasts.
const LOCK_WINDOW_MS: i64 = 15 * 60 * 1000;
/// Live links kept at once.
const MAX_LINKS: usize = 64;
/// The settings file in the data dir (address and PIN hash).
const CONFIG_FILE: &str = "phone-approval.json";
/// Header the chat channels (Telegram, Discord) send on every daemon request.
pub const CHANNEL_HEADER: &str = "x-praxi-channel";
/// A chat channel tried to approve a money ask.
pub const PHONE_APPROVAL_REQUIRED: &str = "phone_approval_required";
/// Largest form body the page accepts.
pub(crate) const MAX_FORM: usize = 4 * 1024;
/// PIN length bounds (digits).
const PIN_MIN: usize = 6;
const PIN_MAX: usize = 12;

/// Stable error codes.
pub mod codes {
    /// Phone approval is not set up.
    pub const OFF: &str = "phone_approval_off";
    /// Too many wrong PINs: phone approval is locked for a while.
    pub const LOCKED: &str = "phone_approval_locked";
    /// The approval is not a money ask (only money asks get a phone link).
    pub const NOT_MONEY: &str = "not_a_money_ask";
    /// The PIN or the address is not acceptable.
    pub const INVALID: &str = "phone_approval_invalid";
}

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    /// `https://host[:port]`, no path.
    address: String,
    /// Argon2id PHC string of the PIN.
    pin_hash: String,
}

struct Link {
    call_id: ToolCallId,
    digest: [u8; 32],
    expires_ms: i64,
    attempts: u8,
    /// Set by a right PIN; the decision form must carry it back.
    confirm: Option<[u8; 32]>,
}

/// Phone approval state of one daemon: the settings (read lazily) and the live links.
#[derive(Default)]
pub(crate) struct PhoneBook {
    loaded: bool,
    config: Option<Stored>,
    /// SHA-256 of the nonce → link. The nonce itself is never kept.
    links: HashMap<[u8; 32], Link>,
    failures: VecDeque<i64>,
    locked_until_ms: i64,
    /// Test clock offset.
    #[cfg(test)]
    pub(crate) skew_ms: i64,
}

impl PhoneBook {
    // `self` carries the test clock offset; release builds read the real clock only.
    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn now(&self) -> i64 {
        #[cfg(test)]
        let skew = self.skew_ms;
        #[cfg(not(test))]
        let skew = 0;
        storage::now_ms() + skew
    }

    fn config(&mut self, s: &AppState) -> Option<Stored> {
        if !self.loaded {
            self.loaded = true;
            self.config = std::fs::read(s.data_dir().join(CONFIG_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
        }
        self.config.clone()
    }

    fn locked(&self, now: i64) -> bool {
        self.locked_until_ms > now
    }

    fn drop_all(&mut self) {
        self.links.clear();
    }

    /// Record a wrong PIN; `true` when this locked phone approval.
    fn wrong_pin(&mut self, now: i64) -> bool {
        self.failures.push_back(now);
        while self
            .failures
            .front()
            .is_some_and(|t| now - *t >= LOCK_WINDOW_MS)
        {
            self.failures.pop_front();
        }
        if self.failures.len() >= LOCK_FAILURES {
            self.locked_until_ms = now + LOCK_WINDOW_MS;
            self.failures.clear();
            self.drop_all();
            return true;
        }
        false
    }
}

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS random source");
    b
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn audit(s: &AppState, action: &str, detail: &serde_json::Value) {
    let _ = s
        .storage()
        .append_audit(s.user_id(), None, "phone", action, detail);
}

/// The digest an ask is bound to: everything the owner is shown and the loop will run.
fn ask_digest(a: &ApprovalDto) -> [u8; 32] {
    sha(json!({
        "call_id": a.call_id,
        "run_id": a.run_id,
        "tool": a.tool_name,
        "summary": a.summary,
        "input": a.input,
        "details": a.details,
    })
    .to_string()
    .as_bytes())
}

/// The pending money ask `call`, if it still waits.
fn pending_money_ask(s: &AppState, call: ToolCallId) -> Result<Option<ApprovalDto>, ApiError> {
    if !s.agent().approval_is_money(call) {
        return Ok(None);
    }
    for run in s
        .storage()
        .list_runs_by_state(task_runtime::RunState::WaitingApproval.as_str())?
    {
        if run.user_id != s.user_id() {
            continue;
        }
        if let Some(a) = crate::routes::run_approvals(s, &run)?
            .into_iter()
            .find(|a| a.call_id == call.0 && a.decision.is_none())
        {
            let money = a
                .details
                .as_ref()
                .and_then(|d| d.get("money_movement"))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            return Ok(money.then_some(a));
        }
    }
    Ok(None)
}

/// Whether a browser `Origin` is the configured public address (the page's own form posts).
pub(crate) fn origin_is_address(s: &AppState, origin: &str) -> bool {
    s.phone_book()
        .lock()
        .config(s)
        .is_some_and(|c| c.address == origin.trim_end_matches('/'))
}

/// `GET /v1/phone-approval/p/{nonce}` and its two POSTs: the routes the guard lets through
/// without the bearer.
pub(crate) fn is_page_route(method: &axum::http::Method, path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/v1/phone-approval/p/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let nonce = parts.next().unwrap_or_default();
    if nonce.is_empty() || nonce.len() > 64 {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => method == axum::http::Method::GET,
        (Some("unlock" | "decide"), None) => method == axum::http::Method::POST,
        _ => false,
    }
}

// ------------------------------------------------------------------ settings

/// Whether phone approval is on.
#[derive(Debug, Serialize, ToSchema)]
pub struct PhoneApprovalStatus {
    /// A PIN and an address are set.
    pub enabled: bool,
    /// The public address links point to (`https://host[:port]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Too many wrong PINs: no link works until then (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_until_ms: Option<i64>,
}

/// Turn phone approval on or change it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PhoneApprovalSetup {
    /// Public https address of the owner's tunnel to this daemon (origin only, no path).
    pub address: String,
    /// 6 to 12 digits. Required the first time; omitted keeps the current PIN.
    #[serde(default)]
    pub pin: Option<String>,
}

fn status_of(s: &AppState) -> PhoneApprovalStatus {
    let mut book = s.phone_book().lock();
    let now = book.now();
    let config = book.config(s);
    PhoneApprovalStatus {
        enabled: config.is_some(),
        address: config.map(|c| c.address),
        locked_until_ms: book.locked(now).then_some(book.locked_until_ms),
    }
}

fn invalid(msg: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, codes::INVALID, msg)
}

/// `https://host[:port]` (or `http://` on a loopback host, for trying it out).
fn normalize_address(raw: &str) -> Result<String, ApiError> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| invalid("the address must be a full https address"))?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("the address has no host"))?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid(
            "the address must be https://host (no path, query or login)",
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn pin_ok(pin: &str) -> bool {
    (PIN_MIN..=PIN_MAX).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit())
}

fn hash_pin(pin: &str) -> Result<String, ApiError> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(&random32()[..16]).map_err(ApiError::internal)?;
    argon2::Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(ApiError::internal)
}

fn verify_pin(hash: &str, pin: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    pin_ok(pin)
        && PasswordHash::new(hash).is_ok_and(|h| {
            argon2::Argon2::default()
                .verify_password(pin.as_bytes(), &h)
                .is_ok()
        })
}

/// Phone approval settings.
#[utoipa::path(get, path = "/v1/phone-approval", tag = "approvals", security(("bearer" = [])),
    responses((status = 200, body = PhoneApprovalStatus)))]
pub(crate) async fn get_phone_approval(State(s): State<AppState>) -> Json<PhoneApprovalStatus> {
    Json(status_of(&s))
}

/// Turn phone approval on, or change the address or the PIN. Any change drops every open link.
#[utoipa::path(put, path = "/v1/phone-approval", tag = "approvals", security(("bearer" = [])),
    request_body = PhoneApprovalSetup,
    responses((status = 200, body = PhoneApprovalStatus), (status = 422, body = ErrorBody)))]
pub(crate) async fn put_phone_approval(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<PhoneApprovalSetup>,
) -> Result<Json<PhoneApprovalStatus>, ApiError> {
    let address = normalize_address(&input.address)?;
    let current = s.phone_book().lock().config(&s);
    let pin_hash = match (input.pin.as_deref(), current) {
        (Some(pin), _) if pin_ok(pin) => {
            let pin = pin.to_owned();
            tokio::task::spawn_blocking(move || hash_pin(&pin))
                .await
                .map_err(ApiError::internal)??
        }
        (Some(_), _) => return Err(invalid("the PIN must be 6 to 12 digits")),
        (None, Some(c)) => c.pin_hash,
        (None, None) => return Err(invalid("choose a PIN of 6 to 12 digits")),
    };
    let stored = Stored { address, pin_hash };
    let bytes = serde_json::to_string(&stored).map_err(ApiError::internal)?;
    crate::auth::write_private(&s.data_dir().join(CONFIG_FILE), &bytes)
        .map_err(ApiError::internal)?;
    {
        let mut book = s.phone_book().lock();
        book.loaded = true;
        book.config = Some(stored.clone());
        book.drop_all();
        book.failures.clear();
        book.locked_until_ms = 0;
    }
    audit(
        &s,
        "phone_approval.configured",
        &json!({"address": stored.address, "pin_changed": input.pin.is_some()}),
    );
    Ok(Json(status_of(&s)))
}

/// Turn phone approval off: the PIN is deleted and every open link stops working.
#[utoipa::path(delete, path = "/v1/phone-approval", tag = "approvals", security(("bearer" = [])),
    responses((status = 200, body = PhoneApprovalStatus)))]
pub(crate) async fn delete_phone_approval(
    State(s): State<AppState>,
) -> Result<Json<PhoneApprovalStatus>, ApiError> {
    match std::fs::remove_file(s.data_dir().join(CONFIG_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ApiError::internal(e)),
    }
    {
        let mut book = s.phone_book().lock();
        book.loaded = true;
        book.config = None;
        book.drop_all();
    }
    audit(&s, "phone_approval.disabled", &json!({}));
    Ok(Json(status_of(&s)))
}

// ------------------------------------------------------------------ links

/// Which ask a phone link is for.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PhoneLinkRequest {
    /// The pending money ask (`call_id` of its `approval_needed` event).
    pub call_id: Uuid,
}

/// A one-time phone link.
#[derive(Debug, Serialize, ToSchema)]
pub struct PhoneLink {
    /// Open on the phone; it works once, until `expires_at_ms`, and needs the PIN.
    pub url: String,
    /// When the link stops working (Unix ms).
    pub expires_at_ms: i64,
}

/// Mint a one-time phone approval link for a pending money ask (owner key only). A new link
/// for the same ask replaces the old one.
#[utoipa::path(post, path = "/v1/phone-approval/links", tag = "approvals",
    security(("bearer" = [])), request_body = PhoneLinkRequest,
    responses((status = 200, body = PhoneLink), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn create_phone_link(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<PhoneLinkRequest>,
) -> Result<Json<PhoneLink>, ApiError> {
    let call = ToolCallId(input.call_id);
    let Some(config) = s.phone_book().lock().config(&s) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            codes::OFF,
            "phone approval is off: turn it on in the app",
        ));
    };
    if !s.agent().is_pending(call) {
        return Err(ApiError::not_found(format!("approval {}", input.call_id)));
    }
    let Some(ask) = pending_money_ask(&s, call)? else {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            codes::NOT_MONEY,
            "only a payment can be approved from the phone",
        ));
    };
    let nonce = random32();
    let mut book = s.phone_book().lock();
    let now = book.now();
    if book.locked(now) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            codes::LOCKED,
            "too many wrong PINs: phone approval is locked for a while",
        ));
    }
    book.links
        .retain(|_, l| l.call_id != call && l.expires_ms > now);
    if book.links.len() >= MAX_LINKS {
        let oldest = book
            .links
            .iter()
            .min_by_key(|(_, l)| l.expires_ms)
            .map(|(k, _)| *k);
        if let Some(k) = oldest {
            book.links.remove(&k);
        }
    }
    let expires_ms = now + LINK_TTL_MS;
    book.links.insert(
        sha(&nonce),
        Link {
            call_id: call,
            digest: ask_digest(&ask),
            expires_ms,
            attempts: 0,
            confirm: None,
        },
    );
    drop(book);
    audit(
        &s,
        "phone_approval.link_created",
        &json!({"call_id": input.call_id}),
    );
    Ok(Json(PhoneLink {
        url: format!("{}/v1/phone-approval/p/{}", config.address, b64(&nonce)),
        expires_at_ms: expires_ms,
    }))
}

// ------------------------------------------------------------------ the page

fn nonce_key(raw: &str) -> Option<[u8; 32]> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    (bytes.len() == 32).then(|| sha(&bytes))
}

/// The live link for `nonce`, as `(call, digest)`; an expired one is removed.
fn live_link(s: &AppState, key: &[u8; 32]) -> Option<(ToolCallId, [u8; 32])> {
    let mut book = s.phone_book().lock();
    let now = book.now();
    if book.locked(now) {
        return None;
    }
    match book.links.get(key) {
        Some(l) if l.expires_ms > now => Some((l.call_id, l.digest)),
        Some(_) => {
            book.links.remove(key);
            None
        }
        None => None,
    }
}

/// The ask behind a link, if it is still the same pending money ask.
fn bound_ask(
    s: &AppState,
    key: &[u8; 32],
    call: ToolCallId,
    digest: &[u8; 32],
) -> Option<ApprovalDto> {
    let ask = pending_money_ask(s, call).ok().flatten();
    match ask {
        Some(a) if ct_eq(&ask_digest(&a), digest) => Some(a),
        _ => {
            s.phone_book().lock().links.remove(key);
            None
        }
    }
}

fn form(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

/// `GET /v1/phone-approval/p/{nonce}`: the facts and the PIN field.
pub(crate) async fn page(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(nonce): Path<String>,
) -> Response {
    let t = Texts::of(&headers);
    let Some(key) = nonce_key(&nonce) else {
        return dead(t);
    };
    let Some((call, digest)) = live_link(&s, &key) else {
        return dead(t);
    };
    let Some(ask) = bound_ask(&s, &key, call, &digest) else {
        return dead(t);
    };
    html(StatusCode::OK, t, &pin_form(t, &nonce, &ask, None))
}

/// `POST …/{nonce}/unlock` (form `pin`): a right PIN shows the screenshot and the decision.
pub(crate) async fn unlock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(nonce): Path<String>,
    body: Bytes,
) -> Response {
    let t = Texts::of(&headers);
    let Some(key) = nonce_key(&nonce) else {
        return dead(t);
    };
    let Some((call, digest)) = live_link(&s, &key) else {
        return locked_or_dead(&s, t);
    };
    let Some(ask) = bound_ask(&s, &key, call, &digest) else {
        return dead(t);
    };
    // Take one attempt before the PIN is checked, so parallel guesses cannot exceed the cap.
    let Some(hash) = ({
        let mut book = s.phone_book().lock();
        let hash = book.config(&s).map(|c| c.pin_hash);
        match book.links.get_mut(&key) {
            Some(l) if l.attempts < LINK_ATTEMPTS => {
                l.attempts += 1;
                hash
            }
            _ => {
                book.links.remove(&key);
                None
            }
        }
    }) else {
        return dead(t);
    };
    let pin = form(&body).remove("pin").unwrap_or_default();
    let right = tokio::task::spawn_blocking(move || verify_pin(&hash, pin.trim()))
        .await
        .unwrap_or(false);
    let call_uuid = call.0;
    if !right {
        let (locked, left) = {
            let mut book = s.phone_book().lock();
            let now = book.now();
            let locked = book.wrong_pin(now);
            let left = match book.links.get(&key) {
                Some(l) if l.attempts < LINK_ATTEMPTS => LINK_ATTEMPTS - l.attempts,
                _ => {
                    book.links.remove(&key);
                    0
                }
            };
            (locked, left)
        };
        audit(
            &s,
            if locked {
                "phone_approval.locked"
            } else {
                "phone_approval.wrong_pin"
            },
            &json!({"call_id": call_uuid}),
        );
        if locked {
            return html(StatusCode::TOO_MANY_REQUESTS, t, t.locked);
        }
        if left == 0 {
            return dead(t);
        }
        return html(
            StatusCode::FORBIDDEN,
            t,
            &pin_form(t, &nonce, &ask, Some(left)),
        );
    }
    let confirm = random32();
    {
        let mut book = s.phone_book().lock();
        match book.links.get_mut(&key) {
            Some(l) => l.confirm = Some(confirm),
            None => return dead(t),
        }
    }
    let shot = s.agent().checkout_screenshot(call);
    html(
        StatusCode::OK,
        t,
        &decision_form(t, &nonce, &ask, shot.as_ref(), &b64(&confirm)),
    )
}

/// `POST …/{nonce}/decide` (form `confirm`, `decision`): approve once or deny; the link is gone.
pub(crate) async fn decide(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(nonce): Path<String>,
    body: Bytes,
) -> Response {
    let t = Texts::of(&headers);
    let Some(key) = nonce_key(&nonce) else {
        return dead(t);
    };
    let fields = form(&body);
    let given = fields
        .get("confirm")
        .and_then(|c| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c)
                .ok()
        })
        .unwrap_or_default();
    // Single use: the link is taken out before anything else runs, so a replay finds nothing.
    let link = {
        let mut book = s.phone_book().lock();
        let now = book.now();
        let locked = book.locked(now);
        book.links
            .remove(&key)
            .filter(|l| !locked && l.expires_ms > now)
    };
    let Some(link) = link else {
        return dead(t);
    };
    if !link.confirm.is_some_and(|c| ct_eq(&c, &given)) {
        audit(
            &s,
            "phone_approval.refused",
            &json!({"call_id": link.call_id.0, "why": "no_pin"}),
        );
        return dead(t);
    }
    if bound_ask(&s, &key, link.call_id, &link.digest).is_none() {
        return dead(t);
    }
    let approve = match fields.get("decision").map(String::as_str) {
        Some("approve") => true,
        Some("deny") => false,
        _ => return dead(t),
    };
    let done = if approve {
        s.agent()
            .approve(link.call_id, ApprovalScope::Once, s.user_id())
    } else {
        s.agent().deny(
            link.call_id,
            s.user_id(),
            Some("denied from the phone".into()),
        )
    };
    if done.is_err() {
        return dead(t);
    }
    audit(
        &s,
        if approve {
            "phone_approval.approved"
        } else {
            "phone_approval.denied"
        },
        &json!({"call_id": link.call_id.0}),
    );
    html(
        StatusCode::OK,
        t,
        if approve { t.approved } else { t.denied },
    )
}

fn locked_or_dead(s: &AppState, t: &'static Texts) -> Response {
    let locked = {
        let book = s.phone_book().lock();
        book.locked(book.now())
    };
    if locked {
        html(StatusCode::TOO_MANY_REQUESTS, t, t.locked)
    } else {
        dead(t)
    }
}

fn dead(t: &'static Texts) -> Response {
    html(StatusCode::NOT_FOUND, t, t.dead)
}

// ------------------------------------------------------------------ HTML

/// The page's words. English and Turkish, chosen by the phone browser's language.
pub(crate) struct Texts {
    lang: &'static str,
    title: &'static str,
    merchant: &'static str,
    domain: &'static str,
    amount: &'static str,
    request: &'static str,
    page_claims: &'static str,
    pin: &'static str,
    show: &'static str,
    wrong_pin: &'static str,
    approve: &'static str,
    deny: &'static str,
    once: &'static str,
    approved: &'static str,
    denied: &'static str,
    dead: &'static str,
    locked: &'static str,
    screenshot: &'static str,
}

const EN: Texts = Texts {
    lang: "en",
    title: "Approve a payment",
    merchant: "Merchant",
    domain: "Site",
    amount: "Amount",
    request: "Request",
    page_claims: "Merchant, amount and picture are what the checkout page shows; the site is the address the browser is on.",
    pin: "Phone approval PIN",
    show: "Continue",
    wrong_pin: "Wrong PIN. Tries left on this link:",
    approve: "Approve this payment",
    deny: "Deny",
    once: "Approving allows this one payment only.",
    approved: "Approved. The bot continues with this payment.",
    denied: "Denied. The bot will not make this payment.",
    dead: "This link no longer works. It may have been used, expired after 10 minutes, or the payment was decided in the app. Decide in the Praxi app.",
    locked: "Too many wrong PINs. Phone approval is locked for 15 minutes; decide in the Praxi app.",
    screenshot: "Checkout page",
};

const TR: Texts = Texts {
    lang: "tr",
    title: "Ödemeyi onayla",
    merchant: "Satıcı",
    domain: "Site",
    amount: "Tutar",
    request: "İstek",
    page_claims: "Satıcı, tutar ve görüntü ödeme sayfasının gösterdiğidir; site, tarayıcının açık olduğu adrestir.",
    pin: "Telefondan onay PIN'i",
    show: "Devam",
    wrong_pin: "PIN yanlış. Bu bağlantıda kalan deneme:",
    approve: "Bu ödemeyi onayla",
    deny: "Reddet",
    once: "Onay yalnızca bu tek ödeme için geçerlidir.",
    approved: "Onaylandı. Bot bu ödemeyle devam ediyor.",
    denied: "Reddedildi. Bot bu ödemeyi yapmayacak.",
    dead: "Bu bağlantı artık çalışmıyor. Kullanılmış, 10 dakika dolmuş ya da ödeme uygulamada karara bağlanmış olabilir. Praxi uygulamasında karar verin.",
    locked: "Çok fazla yanlış PIN girildi. Telefondan onay 15 dakika kilitli; Praxi uygulamasında karar verin.",
    screenshot: "Ödeme sayfası",
};

impl Texts {
    pub(crate) fn of(headers: &HeaderMap) -> &'static Self {
        let tr = headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .is_some_and(|first| first.trim().to_ascii_lowercase().starts_with("tr"));
        if tr { &TR } else { &EN }
    }
}

fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn facts(t: &Texts, ask: &ApprovalDto) -> String {
    let checkout = ask.details.as_ref().and_then(|d| d.get("checkout"));
    let fact = |k: &str| {
        checkout
            .and_then(|c| c.get(k))
            .and_then(serde_json::Value::as_str)
            .map(|v| esc(v.chars().take(120).collect::<String>().as_str()))
    };
    let mut rows = String::new();
    for (label, key) in [
        (t.merchant, "merchant"),
        (t.domain, "domain"),
        (t.amount, "amount"),
    ] {
        if let Some(v) = fact(key) {
            let _ = write!(rows, "<dt>{label}</dt><dd>{v}</dd>");
        }
    }
    let summary = esc(&ask.summary.chars().take(300).collect::<String>());
    let _ = write!(rows, "<dt>{}</dt><dd class=s>{summary}</dd>", t.request);
    format!(
        "<dl>{rows}</dl><p class=n>{}</p>",
        if checkout.is_some() {
            t.page_claims
        } else {
            ""
        }
    )
}

fn pin_form(t: &Texts, nonce: &str, ask: &ApprovalDto, left: Option<u8>) -> String {
    let warn = left
        .map(|n| format!("<p class=w>{} {n}</p>", t.wrong_pin))
        .unwrap_or_default();
    format!(
        "<h1>{title}</h1>{facts}{warn}<form method=post action=\"/v1/phone-approval/p/{nonce}/unlock\">\
<label>{pin}<input name=pin type=password inputmode=numeric autocomplete=off pattern=\"[0-9]*\" minlength={PIN_MIN} maxlength={PIN_MAX} required autofocus></label>\
<button>{show}</button></form>",
        title = t.title,
        facts = facts(t, ask),
        nonce = esc(nonce),
        pin = t.pin,
        show = t.show,
    )
}

fn decision_form(
    t: &Texts,
    nonce: &str,
    ask: &ApprovalDto,
    shot: Option<&(Vec<u8>, &'static str)>,
    confirm: &str,
) -> String {
    let picture = shot
        .map(|(bytes, mime)| {
            format!(
                "<figure><img alt=\"{alt}\" src=\"data:{mime};base64,{data}\"><figcaption>{alt}</figcaption></figure>",
                alt = t.screenshot,
                data = base64::engine::general_purpose::STANDARD.encode(bytes),
            )
        })
        .unwrap_or_default();
    let button = |decision: &str, label: &str, class: &str| {
        format!(
            "<form method=post action=\"/v1/phone-approval/p/{nonce}/decide\">\
<input type=hidden name=confirm value=\"{confirm}\"><input type=hidden name=decision value={decision}>\
<button class={class}>{label}</button></form>",
            nonce = esc(nonce),
            confirm = esc(confirm),
        )
    };
    format!(
        "<h1>{title}</h1>{facts}{picture}<p class=n>{once}</p>{approve}{deny}",
        title = t.title,
        facts = facts(t, ask),
        once = t.once,
        approve = button("approve", t.approve, "a"),
        deny = button("deny", t.deny, "d"),
    )
}

const STYLE: &str = "body{font:16px/1.4 system-ui,sans-serif;margin:0;padding:16px;max-width:560px;\
color:#1d1d1f;background:#fafafa}h1{font-size:1.3em}dl{display:grid;grid-template-columns:auto 1fr;gap:4px 12px}\
dt{color:#555}dd{margin:0;font-weight:600;overflow-wrap:anywhere}dd.s{font-weight:400;font-family:monospace;font-size:.85em}\
.n{color:#555;font-size:.9em}.w{color:#b00020}label{display:block;margin:12px 0}input{display:block;width:100%;\
font-size:1.4em;padding:8px;margin-top:4px;box-sizing:border-box}button{width:100%;font-size:1.1em;padding:12px;\
margin:6px 0;border-radius:8px;border:1px solid #888;background:#fff}button.a{background:#0b6b2f;color:#fff;border:0}\
img{max-width:100%;border:1px solid #ccc}figure{margin:12px 0}figcaption{font-size:.85em;color:#555}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}dt,.n,figcaption{color:#aaa}button{background:#222;color:#eee}}";

fn html(status: StatusCode, t: &Texts, body: &str) -> Response {
    let doc = format!(
        "<!doctype html><html lang={lang}><head><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\"><meta name=robots content=noindex>\
<title>{title}</title><style>{STYLE}</style></head><body>{body}</body></html>",
        lang = t.lang,
        title = t.title,
    );
    let mut resp = (status, doc).into_response();
    let h = resp.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "same-origin"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; img-src data:; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    ] {
        h.insert(name, HeaderValue::from_static(value));
    }
    resp
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn addresses_are_https_origins() {
        assert_eq!(
            normalize_address("https://pay.example.org/").unwrap(),
            "https://pay.example.org"
        );
        assert_eq!(
            normalize_address("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        for bad in [
            "http://pay.example.org",
            "https://pay.example.org/sub",
            "https://u:p@pay.example.org",
            "https://pay.example.org/?x=1",
            "pay.example.org",
            "javascript:alert(1)",
        ] {
            assert!(normalize_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pins_are_six_to_twelve_digits_and_hashed() {
        assert!(pin_ok("123456") && pin_ok("123456789012"));
        assert!(!pin_ok("12345") && !pin_ok("1234567890123") && !pin_ok("12345a"));
        let h = hash_pin("246810").unwrap();
        assert!(h.starts_with("$argon2id$"), "{h}");
        assert!(!h.contains("246810"));
        assert!(verify_pin(&h, "246810"));
        assert!(!verify_pin(&h, "246811"));
        assert!(!verify_pin("not a hash", "246810"));
    }

    #[test]
    fn only_the_page_routes_are_public() {
        use axum::http::Method;
        let n = "A".repeat(43);
        assert!(is_page_route(
            &Method::GET,
            &format!("/v1/phone-approval/p/{n}")
        ));
        assert!(is_page_route(
            &Method::POST,
            &format!("/v1/phone-approval/p/{n}/unlock")
        ));
        assert!(is_page_route(
            &Method::POST,
            &format!("/v1/phone-approval/p/{n}/decide")
        ));
        assert!(!is_page_route(
            &Method::POST,
            &format!("/v1/phone-approval/p/{n}")
        ));
        assert!(!is_page_route(
            &Method::GET,
            &format!("/v1/phone-approval/p/{n}/decide")
        ));
        assert!(!is_page_route(
            &Method::POST,
            &format!("/v1/phone-approval/p/{n}/decide/x")
        ));
        assert!(!is_page_route(&Method::GET, "/v1/phone-approval"));
        assert!(!is_page_route(&Method::POST, "/v1/phone-approval/links"));
        assert!(!is_page_route(&Method::GET, "/v1/phone-approval/p/"));
    }

    #[test]
    fn page_text_is_escaped() {
        assert_eq!(
            esc("<b a=\"x\">&'</b>\u{7}"),
            "&lt;b a=&quot;x&quot;&gt;&amp;&#39;&lt;/b&gt;"
        );
    }

    #[test]
    fn three_wrong_pins_per_five_lock() {
        let mut book = PhoneBook::default();
        for i in 0..4 {
            assert!(!book.wrong_pin(i), "{i}");
        }
        assert!(book.wrong_pin(5));
        assert!(book.locked(6));
        assert!(!book.locked(5 + LOCK_WINDOW_MS));
        // Failures older than the window do not count.
        let mut book = PhoneBook::default();
        for i in 0..4 {
            assert!(!book.wrong_pin(i));
        }
        assert!(!book.wrong_pin(LOCK_WINDOW_MS + 10));
    }
}
