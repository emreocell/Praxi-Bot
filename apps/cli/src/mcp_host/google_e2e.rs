//! The owner's whole Google journey against a fake Google, with nothing on the
//! network: connect Gmail, Calendar and Drive with the owner's own client (on
//! this computer and on a server whose redirect is forwarded), where the tokens
//! live, refresh on expiry, sign out and disconnect, a bot reading mail and
//! creating events through the connection, the per-bot switch, and Composio's
//! Gmail next to it.
//!
//! The fake is one loopback server that plays Google's authorization server
//! (metadata, PKCE, a confidential client, no `iss` on the redirect and no new
//! refresh token on a refresh, as Google does) and the three MCP endpoints.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tool_registry::ToolContext;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::tests::{browser_redirect, contains, files_under, query_pairs, wait_auth, wait_for};
use super::*;

/// One journey at a time: they are long, and the quick tests next to them
/// have timeouts of a few hundred milliseconds that a busy machine would trip.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Quick to give up on a failed retry, but patient with a slow machine.
fn policy() -> McpPolicy {
    McpPolicy {
        quarantine_after: 3,
        backoff: mcp_bridge::Backoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(1),
            max_attempts: 1,
        },
        connect_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
    }
}

const CLIENT_ID: &str = "123456789012-praxitest.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "GOCSPX-fake-client-secret-sentinel-31c9";
const SECRET_NAME: &str = "mcp_google_client_secret";
const REFRESH: &str = "1//fake-refresh-token-sentinel-5b2e";
const GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
const CALENDAR_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";

fn access(n: u32) -> String {
    format!("ya29.fake-access-token-sentinel-{n}")
}

/// What one MCP tool call looked like to Google.
#[derive(Debug, Clone)]
struct Call {
    service: String,
    tool: String,
    bearer: Option<String>,
    arguments: Value,
}

#[derive(Default)]
struct State {
    /// `code_challenge` and `redirect_uri` of the authorization request the
    /// test (playing the browser) is about to answer.
    challenge: Option<String>,
    redirect_uri: Option<String>,
    scope: String,
    /// Lifetime of the access token a code exchange hands out.
    first_expires_in: u64,
    issued: u32,
    valid: BTreeSet<String>,
    refresh_valid: bool,
    token_requests: Vec<BTreeMap<String, String>>,
    revocations: Vec<String>,
    /// Google is down for revocations (answers 503 and forgets nothing).
    revoke_down: bool,
    calls: Vec<Call>,
}

struct Google {
    base: String,
    state: Arc<Mutex<State>>,
}

/// Tools each Google MCP service lists (name, writes?). Google lists them all;
/// the catalog's allow list decides which ones a bot may see.
fn tools_of(service: &str) -> Vec<(&'static str, bool)> {
    match service {
        "gmail" => vec![
            ("search_threads", false),
            ("get_thread", false),
            ("list_labels", false),
            ("create_draft", true),
            ("send_message", true),
        ],
        "calendar" => vec![
            ("list_calendars", false),
            ("list_events", false),
            ("create_event", true),
            ("delete_event", true),
        ],
        _ => vec![("list_recent_files", false), ("delete_file", true)],
    }
}

fn pairs(body: &[u8]) -> BTreeMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(body)))
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn json_response(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

impl Google {
    fn token(&self, request: &Request) -> ResponseTemplate {
        let mut form = pairs(&request.body);
        // Google takes the client's credentials in the body or as HTTP Basic.
        if let Some((id, secret)) = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|v| v.split_once(':').map(|(i, s)| (i.to_owned(), s.to_owned())))
        {
            form.entry("client_id".to_owned()).or_insert(id);
            form.entry("client_secret".to_owned()).or_insert(secret);
        }
        let mut state = self.state.lock();
        state.token_requests.push(form.clone());
        if form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
            || form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
        {
            return json_response(401, &json!({"error": "invalid_client"}));
        }
        let grant = form.get("grant_type").map(String::as_str);
        let fresh = if grant == Some("authorization_code") {
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(verifier.as_bytes()));
            let good = form.get("code").map(String::as_str) == Some("code-1")
                && state.challenge.as_deref() == Some(challenge.as_str())
                && form.get("redirect_uri") == state.redirect_uri.as_ref();
            if !good {
                return json_response(400, &json!({"error": "invalid_grant"}));
            }
            state.refresh_valid = true;
            true
        } else if grant == Some("refresh_token") {
            if !state.refresh_valid
                || form.get("refresh_token").map(String::as_str) != Some(REFRESH)
            {
                return json_response(
                    400,
                    &json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
                );
            }
            false
        } else {
            return json_response(400, &json!({"error": "unsupported_grant_type"}));
        };
        state.issued += 1;
        let token = access(state.issued);
        state.valid.insert(token.clone());
        let expires_in = if fresh { state.first_expires_in } else { 3599 };
        let mut body = json!({
            "access_token": token, "expires_in": expires_in, "token_type": "Bearer",
            "scope": state.scope,
        });
        // Google hands out the refresh token once, with the code, never again.
        if fresh {
            body["refresh_token"] = json!(REFRESH);
        }
        json_response(200, &body)
    }

    fn mcp(&self, service: &str, request: &Request) -> ResponseTemplate {
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned);
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let method = body["method"].as_str().unwrap_or_default();
        let mut state = self.state.lock();
        if method == "tools/call" {
            state.calls.push(Call {
                service: service.to_owned(),
                tool: body["params"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                bearer: bearer.clone(),
                arguments: body["params"]["arguments"].clone(),
            });
        }
        let authorized = bearer.as_ref().is_some_and(|b| state.valid.contains(b));
        if !authorized {
            return ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp/{service}\"",
                    self.base
                ),
            );
        }
        if body.get("id").is_none() {
            return ResponseTemplate::new(if method.starts_with("notifications/") {
                202
            } else {
                405
            });
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                "serverInfo": {"name": format!("fake-google-{service}"), "version": "1"},
            }),
            "server/discover" => json!({
                "resultType": "complete", "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}}, "ttlMs": 0, "cacheScope": "private",
                "_meta": {"io.modelcontextprotocol/serverInfo": {"name": format!("fake-google-{service}"), "version": "1"}},
            }),
            "tools/list" => json!({
                "resultType": "complete", "ttlMs": 0, "cacheScope": "private",
                "tools": tools_of(service).iter().map(|(name, writes)| json!({
                    "name": name, "description": format!("{name} ({service})"),
                    "inputSchema": {"type": "object"},
                    "annotations": {"readOnlyHint": !writes},
                })).collect::<Vec<_>>(),
            }),
            "tools/call" => {
                let tool = body["params"]["name"].as_str().unwrap_or_default();
                let needs = match tool {
                    "send_message" => Some(GMAIL_SEND),
                    "create_event" => Some(CALENDAR_EVENTS),
                    _ => None,
                };
                if needs.is_some_and(|scope| !state.scope.split(' ').any(|s| s == scope)) {
                    return json_response(
                        403,
                        &json!({"error": {"code": 403, "status": "PERMISSION_DENIED",
                            "message": "Request had insufficient authentication scopes."}}),
                    );
                }
                let text = match tool {
                    "search_threads" => json!({"threads": (1..=5)
                        .map(|i| json!({"id": format!("t{i}"), "subject": format!("Invoice {i}")}))
                        .collect::<Vec<_>>()}),
                    "list_calendars" => json!({"calendars": [
                        {"id": "primary", "summary": "Work"}, {"id": "home", "summary": "Home"}]}),
                    "list_recent_files" => json!({"files": [{"id": "f1", "name": "Plan.docx"}]}),
                    "send_message" => json!({"id": "sent-1", "labelIds": ["SENT"]}),
                    "create_event" => json!({"id": "ev-1", "status": "confirmed"}),
                    other => json!({"ok": other}),
                };
                json!({"resultType": "complete", "content": [{"type": "text", "text": text.to_string()}]})
            }
            _ => json!({"resultType": "complete"}),
        };
        json_response(
            200,
            &json!({"jsonrpc": "2.0", "id": body["id"], "result": result}),
        )
    }
}

impl Respond for Google {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path().to_owned();
        if let Some(service) = path.strip_prefix("/mcp/") {
            return self.mcp(service, request);
        }
        if let Some(suffix) = path.strip_prefix("/.well-known/oauth-protected-resource") {
            return json_response(
                200,
                &json!({"resource": format!("{}{suffix}", self.base),
                    "authorization_servers": [self.base]}),
            );
        }
        if path.contains("oauth-authorization-server") || path.contains("openid-configuration") {
            return json_response(
                200,
                &json!({
                    "issuer": self.base,
                    "authorization_endpoint": format!("{}/authorize", self.base),
                    "token_endpoint": format!("{}/token", self.base),
                    "revocation_endpoint": format!("{}/revoke", self.base),
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code", "refresh_token"],
                    "code_challenge_methods_supported": ["plain", "S256"],
                    "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
                }),
            );
        }
        match path.as_str() {
            "/token" => self.token(request),
            "/revoke" => {
                let token = pairs(&request.body).remove("token").unwrap_or_default();
                let mut state = self.state.lock();
                if state.revoke_down {
                    return ResponseTemplate::new(503);
                }
                if token == REFRESH {
                    state.refresh_valid = false;
                    state.valid.clear();
                } else {
                    state.valid.remove(&token);
                }
                state.revocations.push(token);
                json_response(200, &json!({}))
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

struct Rig {
    _alone: tokio::sync::MutexGuard<'static, ()>,
    google: MockServer,
    state: Arc<Mutex<State>>,
    dir: tempfile::TempDir,
    store: Arc<secret_store::MemoryStore>,
    registry: ToolRegistry,
    redactor: agent_core::SharedRedactor,
    host: Arc<McpHost>,
}

impl Rig {
    async fn new() -> Self {
        let alone = ONE_AT_A_TIME.lock().await;
        let google = MockServer::start().await;
        let state = Arc::new(Mutex::new(State {
            first_expires_in: 3599,
            ..State::default()
        }));
        Mock::given(any())
            .respond_with(Google {
                base: google.uri(),
                state: Arc::clone(&state),
            })
            .mount(&google)
            .await;
        let dir = tempfile::tempdir().unwrap();
        // What the secrets route does with the name the Google card uses.
        secret_store::validate_name(SECRET_NAME).unwrap();
        assert!(!secret_store::is_reserved_name(SECRET_NAME));
        assert!(!secret_store::is_host_credential(SECRET_NAME));
        let store = Arc::new(secret_store::MemoryStore::new());
        store.set(SECRET_NAME, CLIENT_SECRET).unwrap();
        let redactor = agent_core::SharedRedactor::default();
        let registrar: mcp_bridge::oauth::SecretRegistrar = {
            let handle = redactor.clone();
            Arc::new(move |value| handle.register(value))
        };
        let registry = ToolRegistry::new();
        let host = McpHost::start_with_policy(
            dir.path(),
            registry.clone(),
            Some(store.clone()),
            None,
            Some(registrar),
            policy(),
        );
        Self {
            _alone: alone,
            google,
            state,
            dir,
            store,
            registry,
            redactor,
            host,
        }
    }

    fn base(&self) -> String {
        self.google.uri()
    }

    /// The server settings the Google card sends for a catalog entry, with the
    /// fake in place of Google's addresses.
    fn card(&self, id: &str) -> McpServerInput {
        let entry = self
            .host
            .catalog()
            .into_iter()
            .find(|e| e.id == id)
            .expect("catalog entry");
        let client = entry
            .install
            .oauth_client
            .clone()
            .expect("own-client entry");
        let service = id.trim_start_matches("google-");
        serde_json::from_value(json!({
            "name": id,
            "transport": "http",
            "url": format!("{}/mcp/{service}", self.base()),
            "category": entry.category,
            "enabled": true,
            "tool_allow": entry.install.tool_allow,
            "oauth": {
                "client_id": CLIENT_ID,
                "client_secret_ref": SECRET_NAME,
                "issuer": self.base(),
                "scopes": client.scopes,
                "offline_access": client.offline_access,
                "allow_loopback_http": true,
            },
        }))
        .unwrap()
    }

    /// Start a sign-in and answer Google's consent page like the owner's
    /// browser would; returns when the app is connected.
    async fn sign_in(&self, id: &str) {
        let started = self.host.auth_start(id, None).await.unwrap();
        self.consent(&started.authorization_url);
        browser_redirect(
            &started.authorization_url,
            &format!(
                "code=code-1&state={}",
                query_pairs(&started.authorization_url)["state"]
            ),
        )
        .await
        .unwrap();
        wait_for(&self.host, id, McpStatus::Connected).await;
        wait_auth(&self.host, id, McpAuthState::SignedIn).await;
    }

    /// What Google remembers from the consent page of `authorization_url`.
    fn consent(&self, authorization_url: &str) {
        let params = query_pairs(authorization_url);
        let mut state = self.state.lock();
        state.challenge = Some(params["code_challenge"].clone());
        state.redirect_uri = Some(params["redirect_uri"].clone());
        state.scope = params["scope"].clone();
    }

    fn stored_tokens(&self, id: &str) -> Option<String> {
        self.store
            .get(&format!("mcp/{id}/oauth"))
            .unwrap()
            .map(|v| v.expose().to_owned())
    }

    fn calls(&self) -> Vec<Call> {
        self.state.lock().calls.clone()
    }
}

fn bot(id: &str) -> ToolContext {
    ToolContext {
        run_id: "run-1".into(),
        bot_id: Some(id.into()),
        ..ToolContext::default()
    }
}

fn names(registry: &ToolRegistry, bot_id: &str) -> Vec<String> {
    let mut out: Vec<String> = registry
        .list()
        .into_iter()
        .filter(|spec| {
            registry
                .get(&spec.name)
                .is_some_and(|t| t.visible_to(Some(bot_id)))
        })
        .map(|spec| spec.name)
        .collect();
    out.sort();
    out
}

fn text_of(out: &tool_registry::ToolOutput) -> String {
    out.content
        .iter()
        .map(|part| match part {
            tool_registry::ContentPart::Text { text } => text.clone(),
            tool_registry::ContentPart::Json { data } => data.to_string(),
            tool_registry::ContentPart::Binary { .. } => String::new(),
        })
        .collect()
}

/// Connect from the app on this computer: the browser opens here and Google
/// redirects to a one-shot loopback listener of the daemon.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One owner journey from card to bot.
async fn connect_on_this_computer_then_a_bot_reads_mail_and_the_switch_limits_it() {
    let rig = Rig::new().await;
    let gmail = rig.host.add(rig.card("google-gmail")).await.unwrap();
    assert!(gmail.oauth.as_ref().is_some_and(|o| o.offline_access));
    // Added but not signed in: waiting for the owner, not an error.
    wait_for(&rig.host, "google-gmail", McpStatus::AuthRequired).await;
    assert_eq!(names(&rig.registry, "bot-a").len(), 0);

    // The consent page: the owner's client, read-only scope, a refresh token
    // asked for, PKCE, a redirect back to this computer, and no secret.
    let started = rig.host.auth_start("google-gmail", None).await.unwrap();
    assert!(!started.relayed);
    let params = query_pairs(&started.authorization_url);
    assert!(
        started
            .authorization_url
            .starts_with(&format!("{}/authorize?", rig.base()))
    );
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(
        params["scope"],
        "https://www.googleapis.com/auth/gmail.readonly"
    );
    assert_eq!(params["access_type"], "offline");
    assert_eq!(params["prompt"], "consent");
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_eq!(params["redirect_uri"], started.redirect_uri);
    assert!(!started.authorization_url.contains(CLIENT_SECRET));
    rig.consent(&started.authorization_url);
    browser_redirect(
        &started.authorization_url,
        &format!("code=code-1&state={}", params["state"]),
    )
    .await
    .unwrap();
    wait_for(&rig.host, "google-gmail", McpStatus::Connected).await;
    wait_auth(&rig.host, "google-gmail", McpAuthState::SignedIn).await;

    // Google got the client secret with the exchange, and only there.
    {
        let state = rig.state.lock();
        let exchange = &state.token_requests[0];
        assert_eq!(exchange["client_secret"], CLIENT_SECRET);
        assert_eq!(exchange["grant_type"], "authorization_code");
    }

    // Read tools only: Google also lists drafts and send, the card lets them not through.
    assert_eq!(
        names(&rig.registry, "bot-a"),
        [
            "mcp:google-gmail/get_thread",
            "mcp:google-gmail/list_labels",
            "mcp:google-gmail/search_threads"
        ]
    );

    // Tokens live in the secret store only; the client secret never leaves it.
    let tokens = rig.stored_tokens("google-gmail").unwrap();
    assert!(tokens.contains(&access(1)) && tokens.contains(REFRESH));
    assert!(!tokens.contains(CLIENT_SECRET));
    assert_eq!(
        rig.store.get(SECRET_NAME).unwrap().unwrap().expose(),
        CLIENT_SECRET
    );
    let listed = serde_json::to_string(&rig.host.list()).unwrap();
    let file = std::fs::read_to_string(rig.dir.path().join(crate::doctor::MCP_FILE)).unwrap();
    for text in [&listed, &file] {
        for secret in [CLIENT_SECRET, REFRESH, &access(1)] {
            assert!(!text.contains(secret), "{secret} leaked into settings");
        }
    }
    assert!(file.contains(SECRET_NAME) && file.contains(CLIENT_ID));
    for path in files_under(rig.dir.path()) {
        let bytes = std::fs::read(&path).unwrap();
        for secret in [CLIENT_SECRET, REFRESH, &access(1)] {
            assert!(
                !contains(&bytes, secret),
                "{secret} written to {}",
                path.display()
            );
        }
    }
    for secret in [CLIENT_SECRET, REFRESH, &access(1)] {
        assert!(
            !rig.redactor
                .redact(&format!("x {secret} y"))
                .contains(secret),
            "{secret} is not masked"
        );
    }

    // The Test button: the first three mails, not the fifth, shown to the owner only.
    let tested = rig.host.test_call("google-gmail").await.unwrap();
    assert_eq!(tested.tool, "search_threads");
    assert_eq!(tested.items.len(), 3);
    assert!(tested.more);

    // A bot searches the mailbox through the connection, with the bearer.
    let search = rig.registry.get("mcp:google-gmail/search_threads").unwrap();
    let out = search
        .call(
            &bot("bot-a"),
            json!({"query": "from:billing", "pageSize": 5}),
        )
        .await
        .unwrap();
    assert!(text_of(&out).contains("Invoice 1"));
    let last = rig.calls().pop().unwrap();
    assert_eq!(last.bearer.as_deref(), Some(access(1).as_str()));
    assert_eq!(last.arguments["query"], "from:billing");

    // Sending is not in the connection: not listed, and a call by name finds nothing.
    assert!(rig.registry.get("mcp:google-gmail/send_message").is_none());

    // The per-bot switch: one bot, then nobody, then everybody again.
    rig.host
        .update("google-gmail", json!({"bots": ["bot-a"]}))
        .await
        .unwrap();
    assert_eq!(names(&rig.registry, "bot-a").len(), 3);
    assert_eq!(names(&rig.registry, "bot-b").len(), 0);
    let refused = search.call(&bot("bot-b"), json!({"query": "x"})).await;
    assert!(
        refused.is_err(),
        "a bot that is switched off reached Google"
    );
    assert!(
        search
            .call(&bot("bot-a"), json!({"query": "x"}))
            .await
            .is_ok()
    );
    rig.host
        .update("google-gmail", json!({"bots": []}))
        .await
        .unwrap();
    assert_eq!(names(&rig.registry, "bot-a").len(), 0);
    assert!(
        search
            .call(&bot("bot-a"), json!({"query": "x"}))
            .await
            .is_err()
    );
    rig.host
        .update("google-gmail", json!({"bots": null}))
        .await
        .unwrap();
    assert_eq!(names(&rig.registry, "bot-b").len(), 3);
    // None of that asked Google for a new sign-in.
    assert_eq!(rig.state.lock().token_requests.len(), 1);
    rig.host.shutdown().await;
}

/// Google ends an access token after an hour: it is renewed with the refresh
/// token (which Google does not repeat), with no new sign-in, also when Google
/// ends it early; and when the owner took Praxi's access away at Google, the
/// app asks to sign in again instead of retrying.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One token lifetime, start to finish.
async fn tokens_are_refreshed_on_expiry_and_a_revoked_sign_in_asks_again() {
    let rig = Rig::new().await;
    // 60 seconds is the refresh window: this one is due six seconds from now.
    rig.state.lock().first_expires_in = 66;
    rig.host.add(rig.card("google-gmail")).await.unwrap();
    rig.sign_in("google-gmail").await;
    let search = rig.registry.get("mcp:google-gmail/search_threads").unwrap();

    // Time passes until the token is inside the refresh window: the next call
    // renews first. (The clock is the real one: the wait is long enough for the
    // whole-second arithmetic and a slow machine only makes the token due sooner.)
    tokio::time::sleep(Duration::from_secs(8)).await;
    search
        .call(&bot("bot-a"), json!({"query": "b"}))
        .await
        .unwrap();
    assert_eq!(
        rig.calls().last().unwrap().bearer.as_deref(),
        Some(access(2).as_str())
    );
    {
        let state = rig.state.lock();
        assert_eq!(state.token_requests.len(), 2);
        let refresh = &state.token_requests[1];
        assert_eq!(refresh["grant_type"], "refresh_token");
        assert_eq!(refresh["refresh_token"], REFRESH);
        assert_eq!(refresh["client_secret"], CLIENT_SECRET);
    }
    // The renewed sign-in keeps the refresh token Google did not repeat.
    let tokens = rig.stored_tokens("google-gmail").unwrap();
    assert!(
        tokens.contains(&access(2)) && tokens.contains(REFRESH),
        "{tokens}"
    );
    assert_eq!(
        rig.host.auth_status("google-gmail").await.unwrap(),
        McpAuthState::SignedIn
    );
    // A third call needs no further renewal (it was good for an hour).
    search
        .call(&bot("bot-a"), json!({"query": "c"}))
        .await
        .unwrap();
    assert_eq!(rig.state.lock().token_requests.len(), 2);

    // Google ends the token early (the clock says it is fine).
    rig.state.lock().valid.remove(&access(2));
    search
        .call(&bot("bot-a"), json!({"query": "d"}))
        .await
        .unwrap();
    assert_eq!(
        rig.calls().last().unwrap().bearer.as_deref(),
        Some(access(3).as_str())
    );
    assert_eq!(rig.state.lock().token_requests.len(), 3);

    // The owner removes Praxi under Google's "Third-party access".
    {
        let mut state = rig.state.lock();
        state.refresh_valid = false;
        state.valid.clear();
    }
    let failed = search.call(&bot("bot-a"), json!({"query": "e"})).await;
    assert!(failed.is_err());
    wait_for(&rig.host, "google-gmail", McpStatus::AuthRequired).await;
    assert_eq!(
        rig.host.auth_status("google-gmail").await.unwrap(),
        McpAuthState::AuthRequired
    );
    // One refused renewal, not a loop; the app is not quarantined.
    assert!(rig.state.lock().token_requests.len() <= 5);
    assert_eq!(
        rig.host
            .manager
            .snapshot("google-gmail")
            .unwrap()
            .consecutive_failures,
        0
    );
    // Signing in again brings it back.
    rig.sign_in("google-gmail").await;
    assert!(
        search
            .call(&bot("bot-a"), json!({"query": "f"}))
            .await
            .is_ok()
            || {
                // The registry holds a fresh tool after the reconnect.
                rig.registry
                    .get("mcp:google-gmail/search_threads")
                    .unwrap()
                    .call(&bot("bot-a"), json!({"query": "f"}))
                    .await
                    .is_ok()
            }
    );
    rig.host.shutdown().await;
}

/// Sign out forgets the sign-in here and tells Google to end it; disconnect
/// removes the app; the shared client secret goes with the last app only.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One teardown journey.
async fn sign_out_revokes_at_google_and_disconnect_forgets_the_client_with_the_last_app() {
    let rig = Rig::new().await;
    rig.host.add(rig.card("google-gmail")).await.unwrap();
    rig.host.add(rig.card("google-calendar")).await.unwrap();
    rig.sign_in("google-gmail").await;
    // The second app shares the client but is signed in on its own.
    rig.sign_in("google-calendar").await;
    assert_eq!(rig.state.lock().token_requests.len(), 2);
    let kept = rig.stored_tokens("google-calendar").unwrap();

    assert_eq!(
        rig.host.auth_logout("google-gmail").await.unwrap(),
        McpAuthState::SignedOut
    );
    assert!(rig.stored_tokens("google-gmail").is_none());
    assert!(
        rig.registry
            .get("mcp:google-gmail/search_threads")
            .is_none()
    );
    // The other app is untouched.
    assert_eq!(rig.stored_tokens("google-calendar").unwrap(), kept);
    assert!(
        rig.registry
            .get("mcp:google-calendar/list_events")
            .is_some()
    );
    // Google was told: its refresh token no longer works anywhere.
    assert_eq!(
        rig.state.lock().revocations,
        [REFRESH],
        "sign out did not revoke at Google"
    );

    // Disconnect with the secret: the calendar still uses it.
    rig.host
        .remove_forgetting_client_secret("google-gmail")
        .await
        .unwrap();
    assert!(rig.store.get(SECRET_NAME).unwrap().is_some());
    // Google cannot be reached for the revocation: the owner is still signed out.
    rig.state.lock().revoke_down = true;
    rig.host
        .remove_forgetting_client_secret("google-calendar")
        .await
        .unwrap();
    assert_eq!(rig.state.lock().revocations, [REFRESH]);
    assert!(rig.store.get(SECRET_NAME).unwrap().is_none());
    assert!(rig.stored_tokens("google-calendar").is_none());
    assert_eq!(rig.host.list().len(), 0);
    assert_eq!(rig.registry.list().len(), 0);
    for path in files_under(rig.dir.path()) {
        let bytes = std::fs::read(&path).unwrap();
        for secret in [CLIENT_SECRET, REFRESH] {
            assert!(
                !contains(&bytes, secret),
                "{secret} left in {}",
                path.display()
            );
        }
    }
    rig.host.shutdown().await;
}

/// A server has no browser: Google redirects to the owner's own computer, the
/// desktop app forwards that address, and the server finishes the sign-in. A
/// refusal on Google's page (the owner pressed Cancel) is a clear failure that
/// can be tried again.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One remote journey: refusal, then success.
async fn on_a_server_the_forwarded_redirect_finishes_the_sign_in() {
    let rig = Rig::new().await;
    rig.host.add(rig.card("google-calendar")).await.unwrap();
    wait_for(&rig.host, "google-calendar", McpStatus::AuthRequired).await;
    // The desktop app picks a free port on the owner's computer.
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };

    // Ports of well-known services are refused outright.
    assert!(
        rig.host
            .auth_start("google-calendar", Some(80))
            .await
            .is_err()
    );

    // The owner presses Cancel on Google's consent page.
    let started = rig
        .host
        .auth_start("google-calendar", Some(port))
        .await
        .unwrap();
    assert!(started.relayed);
    assert_eq!(
        started.redirect_uri,
        format!("http://127.0.0.1:{port}/callback")
    );
    let params = query_pairs(&started.authorization_url);
    assert_eq!(params["redirect_uri"], started.redirect_uri);
    assert!(
        params["scope"]
            .split(' ')
            .all(|s| s.ends_with(".readonly") || s.ends_with("freebusy")),
        "{}",
        params["scope"]
    );
    // The server opened no listener of its own for this flow.
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    let denied = rig
        .host
        .auth_callback(
            "google-calendar",
            &format!(
                "{}?error=access_denied&state={}",
                started.redirect_uri, params["state"]
            ),
        )
        .await;
    let message = format!("{:?}", denied.unwrap_err());
    // The words the sign-in card turns into "you declined on Google's page";
    // Google's own text is never repeated.
    assert!(message.contains("refused the request"), "{message}");
    assert!(!message.contains("access_denied"), "{message}");
    assert_ne!(
        rig.host.auth_status("google-calendar").await.unwrap(),
        McpAuthState::SignedIn
    );
    assert!(rig.stored_tokens("google-calendar").is_none());
    assert_eq!(rig.state.lock().token_requests.len(), 0);

    // Try again, this time the owner agrees.
    let started = rig
        .host
        .auth_start("google-calendar", Some(port))
        .await
        .unwrap();
    rig.consent(&started.authorization_url);
    let state = query_pairs(&started.authorization_url)["state"].clone();
    let state = rig
        .host
        .auth_callback(
            "google-calendar",
            &format!(
                "{}?code=code-1&state={state}&scope=ignored",
                started.redirect_uri
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("{e:?}; Google saw {:?}", rig.state.lock().token_requests));
    assert_eq!(state, McpAuthState::SignedIn);
    wait_for(&rig.host, "google-calendar", McpStatus::Connected).await;
    // Google checked the redirect address it saw at the start of the flow.
    assert_eq!(
        rig.state.lock().token_requests[0]["redirect_uri"],
        format!("http://127.0.0.1:{port}/callback")
    );
    let tested = rig.host.test_call("google-calendar").await.unwrap();
    assert_eq!(tested.items, ["Work", "Home"]);
    rig.host.shutdown().await;
}

/// The cards ask for read-only access. Creating an event or sending mail is a
/// choice the owner makes in the app's settings: the tool is let through, the
/// sign-in is widened and renewed, and only then does Google accept the call.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One owner decision, before and after.
async fn creating_events_and_sending_mail_need_the_owner_to_widen_the_card() {
    let rig = Rig::new().await;
    rig.host.add(rig.card("google-calendar")).await.unwrap();
    rig.host.add(rig.card("google-gmail")).await.unwrap();
    rig.sign_in("google-calendar").await;
    rig.sign_in("google-gmail").await;
    assert!(
        rig.registry
            .get("mcp:google-calendar/create_event")
            .is_none()
    );
    assert!(rig.registry.get("mcp:google-gmail/send_message").is_none());

    // Let the tools through. The token still carries read-only scopes, so
    // Google refuses, and the bot is told why instead of retrying.
    rig.host
        .update(
            "google-calendar",
            json!({"tool_allow": ["list_calendars", "list_events", "create_event"]}),
        )
        .await
        .unwrap();
    wait_for(&rig.host, "google-calendar", McpStatus::Connected).await;
    // A tool the owner never saw is held back until they approve it, also when
    // they just let it through: the card says so instead of hiding it.
    assert!(
        rig.registry
            .get("mcp:google-calendar/create_event")
            .is_none()
    );
    let held = rig
        .host
        .list()
        .into_iter()
        .find(|dto| dto.name == "google-calendar")
        .unwrap()
        .changed_tools;
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(held[0].new_tool && held[0].tool == "create_event");
    rig.host
        .approve_tool_changes("google-calendar", "create_event", &held[0].hash)
        .await
        .unwrap();
    let create = rig
        .registry
        .get("mcp:google-calendar/create_event")
        .unwrap();
    // A write is never "read": the approval rules decide, not the server's hint.
    assert_ne!(create.spec().effect, tool_registry::Effect::Read);
    let before = rig.state.lock().token_requests.len();
    let refused = create
        .call(
            &bot("bot-a"),
            json!({"summary": "Lunch", "start": "2026-12-01T12:00:00Z"}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("403") || refused.to_string().contains("scope"),
        "{refused}"
    );
    assert_eq!(
        rig.state.lock().token_requests.len(),
        before,
        "a 403 is not a reason to sign in again"
    );

    // Widen the scope and sign in once more: the event is created.
    let mut wider = rig.card("google-calendar");
    wider
        .oauth
        .as_mut()
        .unwrap()
        .scopes
        .push(CALENDAR_EVENTS.to_owned());
    rig.host
        .update("google-calendar", json!({"oauth": wider.oauth}))
        .await
        .unwrap();
    rig.host.auth_logout("google-calendar").await.unwrap();
    rig.host.reconnect("google-calendar").await.ok();
    rig.sign_in("google-calendar").await;
    let create = rig
        .registry
        .get("mcp:google-calendar/create_event")
        .unwrap();
    let out = create
        .call(
            &bot("bot-a"),
            json!({"summary": "Lunch", "start": "2026-12-01T12:00:00Z"}),
        )
        .await
        .unwrap();
    assert!(text_of(&out).contains("ev-1"));
    let call = rig.calls().pop().unwrap();
    assert_eq!(
        (call.service.as_str(), call.tool.as_str()),
        ("calendar", "create_event")
    );
    assert_eq!(call.arguments["summary"], "Lunch");
    rig.host.shutdown().await;
}

const COMPOSIO_KEY: &str = "ak_candidate_SECRETKEY_0123456789";

struct NoAudit;

impl composio::Audit for NoAudit {
    fn record(&self, _action: &str, _detail: Value) -> Result<(), String> {
        Ok(())
    }
}

/// A fake Composio with Gmail connected (ACTIVE), as the Apps tile leaves it.
async fn composio_gmail(rig: &Rig) -> (composio::ComposioService, MockServer) {
    use wiremock::matchers::{method, path, query_param};
    let server = MockServer::start().await;
    let give = |status: u16, body: Value| ResponseTemplate::new(status).set_body_json(body);
    Mock::given(path("/toolkits"))
        .and(query_param("search", "gmail"))
        .respond_with(give(
            200,
            json!({"items": [{"slug": "gmail", "name": "Gmail"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth_configs"))
        .respond_with(give(200, json!({"items": [{"id": "ac_1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/connected_accounts/link"))
        .respond_with(give(
            201,
            json!({"link_token": "lt_secret_link_token",
                "redirect_url": "https://connect.composio.dev/link/lk_1",
                "expires_at": "2030-01-01T00:00:00Z", "connected_account_id": "ca_secret_conn_1"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/connected_accounts"))
        .respond_with(give(
            200,
            json!({"items": [{"id": "ca_secret_conn_1", "status": "ACTIVE", "toolkit": {"slug": "gmail"}}]}),
        ))
        .mount(&server)
        .await;
    let action = |slug: &str, get: bool| {
        let mut a = json!({"slug": slug, "description": format!("{slug} via Composio"),
            "toolkit": {"slug": "gmail"}, "version": "20260901_00",
            "input_parameters": {"type": "object", "properties": {"q": {"type": "string"}}}});
        if get {
            a["http_method"] = json!("GET");
        }
        a
    };
    Mock::given(method("GET"))
        .and(path("/tools"))
        .and(query_param("toolkit_slug", "gmail"))
        .respond_with(give(
            200,
            json!({"items": [action("GMAIL_FETCH_EMAILS", true), action("GMAIL_SEND_EMAIL", false)]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tools/execute/GMAIL_SEND_EMAIL"))
        .respond_with(give(
            200,
            json!({"data": {"id": "m1"}, "successful": true, "error": null}),
        ))
        .mount(&server)
        .await;
    let redactor = rig.redactor.clone();
    let service = composio::ComposioService::new(composio::ServiceConfig {
        data_dir: rig.dir.path().to_path_buf(),
        keys: Arc::new(composio::LocalKeySource::new(rig.store.clone())),
        registry: rig.registry.clone(),
        redact: Arc::new(move |text| redactor.redact(text)),
        register_secret: Arc::new(|_| {}),
        audit: Arc::new(NoAudit),
        loopback_base: Some(server.uri()),
    })
    .unwrap();
    service.set_key(COMPOSIO_KEY).await.unwrap();
    service.connect("gmail").await.unwrap();
    service.app_status("gmail").await.unwrap();
    (service, server)
}

/// Google's own Gmail and Composio's Gmail can be connected together: each
/// registers its own tools once, a refresh or reconnect never doubles them,
/// each has its own per-bot switch, and removing one leaves the other alone.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // One side-by-side journey.
async fn composio_gmail_and_google_gmail_side_by_side_without_double_tools() {
    let rig = Rig::new().await;
    rig.host.add(rig.card("google-gmail")).await.unwrap();
    rig.sign_in("google-gmail").await;
    let (composio, _fake) = composio_gmail(&rig).await;

    let tools = |prefix: &str| -> Vec<String> {
        let mut found: Vec<String> = rig
            .registry
            .list()
            .into_iter()
            .map(|spec| spec.name)
            .filter(|name| name.starts_with(prefix))
            .collect();
        found.sort();
        found
    };
    let google = tools("mcp:google-gmail/");
    let apps = tools("app:gmail/");
    assert_eq!(google.len(), 3, "{google:?}");
    assert_eq!(apps, ["app:gmail/fetch_emails", "app:gmail/send_email"]);
    let all: Vec<String> = rig.registry.list().into_iter().map(|s| s.name).collect();
    let unique: BTreeSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "a tool is listed twice: {all:?}");

    // Looking again, renewing or reconnecting does not double anything.
    composio.app_status("gmail").await.unwrap();
    composio.refresh_all().await;
    rig.host.reconnect("google-gmail").await.unwrap();
    wait_for(&rig.host, "google-gmail", McpStatus::Connected).await;
    assert_eq!(tools("mcp:google-gmail/"), google);
    assert_eq!(tools("app:gmail/"), apps);
    assert_eq!(composio.registered_tools("gmail").len(), 2);

    // Each has its own switch: neither follows the other.
    let (a, b) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    composio
        .set_bots("gmail", Some(vec![a.clone()]))
        .await
        .unwrap();
    rig.host
        .update("google-gmail", json!({"bots": [b.clone()]}))
        .await
        .unwrap();
    // (Composio's generic `apps_*` helpers are not tied to a Gmail connection.)
    let tied = |bot: &str| -> Vec<String> {
        let mut found = names(&rig.registry, bot);
        found.retain(|n| n.starts_with("app:") || n.starts_with("mcp:"));
        found
    };
    assert_eq!(tied(&a), ["app:gmail/fetch_emails", "app:gmail/send_email"]);
    assert_eq!(
        tied(&b),
        [
            "mcp:google-gmail/get_thread",
            "mcp:google-gmail/list_labels",
            "mcp:google-gmail/search_threads"
        ]
    );

    // A bot reads through Google's and sends through Composio's; the send is a
    // write, so the approval rules (not a hint) decide whether it asks.
    let send = rig.registry.get("app:gmail/send_email").unwrap();
    assert_ne!(send.spec().effect, tool_registry::Effect::Read);
    let delivered = send.call(&bot(&a), json!({"q": "hello"})).await.unwrap();
    assert!(text_of(&delivered).contains("m1"));
    let search = rig.registry.get("mcp:google-gmail/search_threads").unwrap();
    assert!(search.call(&bot(&b), json!({"query": "x"})).await.is_ok());
    assert!(search.call(&bot(&a), json!({"query": "x"})).await.is_err());

    // Removing either leaves the other working.
    composio.disconnect("gmail").await.unwrap();
    assert_eq!(tools("app:gmail/").len(), 0);
    assert_eq!(tools("mcp:google-gmail/"), google);
    rig.host.auth_logout("google-gmail").await.unwrap();
    assert_eq!(tools("mcp:google-gmail/").len(), 0);
    rig.host.shutdown().await;
}
