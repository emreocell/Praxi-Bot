//! Fake authorization server + MCP endpoint on loopback, shared by the OAuth
//! integration tests (each test binary uses a different subset).
#![allow(dead_code, unreachable_pub)]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use base64::Engine as _;
use mcp_bridge::McpServerConfig;
use mcp_bridge::oauth::{SignIn, SignInOptions};
use parking_lot::Mutex;
use secret_store::{SecretError, SecretStore, SecretValue};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ACCESS: &str = "access-sentinel-e2e-7f3a";
pub const REFRESH: &str = "refresh-sentinel-e2e-91bc";
pub const REFRESHED: &str = "refreshed-access-sentinel-e2e-637c";
pub const ROTATED: &str = "rotated-refresh-sentinel-e2e-0d4e";

#[derive(Default)]
pub struct MemoryStore(pub Mutex<BTreeMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.0.lock().get(name).map(SecretValue::new))
    }
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.0.lock().insert(name.to_owned(), value.to_owned());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        Ok(self.0.lock().remove(name).is_some())
    }
    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.0.lock().keys().cloned().collect())
    }
}

/// What the fake authorization server saw and what it expects.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // independent fixture switches
pub struct Seen {
    /// When set, `POST /register` issues `dcr-client-1` with this secret.
    pub dcr_secret: Option<String>,
    /// Error answers of the token endpoint reflect every submitted value in
    /// `error_description` (a hostile or careless server).
    pub echo_errors: bool,
    /// Authorization server named in the protected-resource metadata
    /// (default: this fake itself).
    pub authorization_server: Option<String>,
    /// `registration_endpoint` advertised in the server metadata.
    pub registration_endpoint: Option<String>,
    /// Answer protected-resource discovery with a 302 to this URL.
    pub discovery_redirect: Option<String>,
    /// Answer `POST /register` with a 307 to this URL.
    pub registration_redirect: Option<String>,
    /// When set, a refresh answer waits for one permit (the test decides
    /// exactly when the refreshed tokens arrive).
    pub refresh_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Refresh requests received (counted before waiting at the gate).
    pub refreshes_received: usize,
    /// Answer refreshes with a new refresh token ([`ROTATED`]).
    pub rotate_refresh: bool,
    pub expires_in: Option<u64>,
    pub refresh_allowed: bool,
    /// When set, speak MCP for this bearer instead of always returning 401.
    pub mcp_bearer: Option<String>,
    pub tool_calls: Vec<Option<String>>,
    pub reject_tool_calls: bool,
    pub client_secret: Option<String>,
    pub cimd_supported: bool,
    /// `code_challenge` of the authorization request (set by the test, which
    /// plays the browser).
    pub challenge: Option<String>,
    /// Decoded form of every token request.
    pub token_requests: Vec<BTreeMap<String, String>>,
    /// Request paths, in order.
    pub paths: Vec<String>,
    /// `Authorization` header of every request to the MCP endpoint itself.
    pub mcp_authorization: Vec<Option<String>>,
    /// Decoded form of every `POST` to the revocation endpoint.
    pub revoke_requests: Vec<BTreeMap<String, String>>,
    /// When set, advertises this RFC 7009 endpoint (default `{base}/revoke`).
    pub revocation_endpoint: Option<String>,
}

pub fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&raw[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pairs(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

/// Authorization server and protected-resource metadata on one loopback port.
#[allow(clippy::too_many_lines)] // One linear fixture: read a request, answer by path.
pub async fn fake_authorization_server(
    advertise_pkce: bool,
) -> (String, Arc<Mutex<Seen>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let task = tokio::spawn({
        let base = base.clone();
        let seen = Arc::clone(&seen);
        async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let base = base.clone();
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head, body) = loop {
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).into_owned();
                        let Some((head, body)) = text.split_once("\r\n\r\n") else {
                            continue;
                        };
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break (head.to_owned(), body.to_owned());
                        }
                    };
                    let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                    seen.lock().paths.push(path.clone());
                    if path == "/mcp" {
                        // By default this endpoint challenges every request;
                        // session tests enable MCP for one expected bearer.
                        let authorization = head.lines().find_map(|l| {
                            let (name, value) = l.split_once(':')?;
                            name.eq_ignore_ascii_case("authorization")
                                .then(|| value.trim().to_owned())
                        });
                        seen.lock().mcp_authorization.push(authorization.clone());
                        let request: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or_default();
                        let method = request["method"].as_str().unwrap_or_default();
                        if method == "tools/call" {
                            seen.lock().tool_calls.push(authorization.clone());
                        }
                        let allowed = seen.lock().mcp_bearer.as_ref().is_some_and(|token| {
                            authorization.as_deref() == Some(format!("Bearer {token}").as_str())
                        });
                        if allowed && !(method == "tools/call" && seen.lock().reject_tool_calls) {
                            if request.get("id").is_none() {
                                let response: &[u8] = if method.starts_with("notifications/") {
                                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                } else {
                                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                };
                                let _ = socket.write_all(response).await;
                                let _ = socket.shutdown().await;
                                return;
                            }
                            let result = match method {
                                "initialize" => serde_json::json!({
                                    "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "oauth-fixture", "version": "1"},
                                }),
                                "server/discover" => serde_json::json!({
                                    "resultType": "complete", "supportedVersions": ["2026-07-28"],
                                    "capabilities": {"tools": {}}, "ttlMs": 0, "cacheScope": "private",
                                    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "oauth-fixture", "version": "1"}},
                                }),
                                "tools/list" => serde_json::json!({
                                    "resultType": "complete", "ttlMs": 0, "cacheScope": "private",
                                    "tools": [{"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}],
                                }),
                                "tools/call" => serde_json::json!({
                                    "resultType": "complete", "content": [{"type": "text", "text": request["params"]["arguments"]["text"].as_str().unwrap_or("echo")}],
                                }),
                                _ => serde_json::json!({"resultType": "complete"}),
                            };
                            let response = serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result}).to_string();
                            let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\n\
                                  Content-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let redirect = if path.contains("oauth-protected-resource") {
                        seen.lock().discovery_redirect.clone()
                    } else if path == "/register" {
                        seen.lock().registration_redirect.clone()
                    } else {
                        None
                    };
                    if let Some(location) = redirect {
                        let _ = socket
                            .write_all(
                                format!(
                                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\n\
                                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                                )
                                .as_bytes(),
                            )
                            .await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let (status, json) = if path.contains("oauth-protected-resource") {
                        let issuer = seen
                            .lock()
                            .authorization_server
                            .clone()
                            .unwrap_or_else(|| base.clone());
                        (
                            "200 OK",
                            serde_json::json!({
                                "resource": format!("{base}/mcp"),
                                "authorization_servers": [issuer],
                            }),
                        )
                    } else if path.contains("oauth-authorization-server")
                        || path.contains("openid-configuration")
                    {
                        let mut metadata = serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "response_types_supported": ["code"],
                            "grant_types_supported": ["authorization_code", "refresh_token"],
                            "authorization_response_iss_parameter_supported": true,
                        });
                        if advertise_pkce {
                            metadata["code_challenge_methods_supported"] =
                                serde_json::json!(["S256"]);
                        }
                        metadata["client_id_metadata_document_supported"] =
                            serde_json::json!(seen.lock().cimd_supported);
                        metadata["token_endpoint_auth_methods_supported"] =
                            serde_json::json!(["client_secret_post"]);
                        if let Some(registration) = seen.lock().registration_endpoint.clone() {
                            metadata["registration_endpoint"] = serde_json::json!(registration);
                        }
                        let revoke = seen
                            .lock()
                            .revocation_endpoint
                            .clone()
                            .unwrap_or_else(|| format!("{base}/revoke"));
                        metadata["revocation_endpoint"] = serde_json::json!(revoke);
                        ("200 OK", metadata)
                    } else if path == "/revoke" {
                        let form = pairs(&body);
                        seen.lock().revoke_requests.push(form);
                        ("200 OK", serde_json::json!({}))
                    } else if path == "/register" && seen.lock().dcr_secret.is_some() {
                        let secret = seen.lock().dcr_secret.clone();
                        (
                            "201 Created",
                            serde_json::json!({
                                "client_id": "dcr-client-1",
                                "client_secret": secret,
                                "redirect_uris": [],
                            }),
                        )
                    } else if path == "/token" {
                        let form = pairs(&body);
                        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
                        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .encode(Sha256::digest(verifier.as_bytes()));
                        let expected = seen.lock().challenge.clone();
                        let secret_valid = seen
                            .lock()
                            .client_secret
                            .as_ref()
                            .is_none_or(|secret| form.get("client_secret") == Some(secret));
                        let valid = secret_valid
                            && form.get("grant_type").map(String::as_str)
                                == Some("authorization_code")
                            && form.get("code").map(String::as_str) == Some("code-1")
                            && Some(challenge) == expected;
                        let refreshing = secret_valid
                            && form.get("grant_type").map(String::as_str) == Some("refresh_token")
                            && form.get("refresh_token").map(String::as_str) == Some(REFRESH)
                            && seen.lock().refresh_allowed;
                        let expires_in = seen.lock().expires_in.unwrap_or(3600);
                        seen.lock().token_requests.push(form);
                        if refreshing {
                            let gate = {
                                let mut seen = seen.lock();
                                seen.refreshes_received += 1;
                                seen.refresh_gate.clone()
                            };
                            if let Some(gate) = gate {
                                gate.acquire().await.unwrap().forget();
                            }
                        }
                        let rotated = seen.lock().rotate_refresh;
                        if valid || refreshing {
                            (
                                "200 OK",
                                serde_json::json!({
                                    "access_token": if refreshing { REFRESHED } else { ACCESS },
                                    "token_type": "Bearer",
                                    "expires_in": if refreshing { 3600 } else { expires_in },
                                    "refresh_token": if refreshing && rotated { ROTATED } else { REFRESH },
                                }),
                            )
                        } else {
                            let reflected = {
                                let seen = seen.lock();
                                seen.echo_errors.then(|| {
                                    seen.token_requests
                                        .last()
                                        .map(|f| f.values().cloned().collect::<Vec<_>>().join(" "))
                                        .unwrap_or_default()
                                })
                            };
                            (
                                "400 Bad Request",
                                match reflected {
                                    Some(all) => serde_json::json!({
                                        "error": "invalid_request",
                                        "error_description": format!("rejected: {all}"),
                                    }),
                                    None => serde_json::json!({"error": "invalid_grant"}),
                                },
                            )
                        }
                    } else {
                        ("404 Not Found", serde_json::json!({"error": "not_found"}))
                    };
                    let body = json.to_string();
                    let _ = socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = socket.shutdown().await;
                });
            }
        }
    });
    (base, seen, task)
}

pub fn options() -> SignInOptions {
    SignInOptions {
        client_id: Some("praxi-test-client".into()),
        allow_loopback_http: true,
        ..SignInOptions::default()
    }
}

/// Play the browser: follow the authorization URL's `redirect_uri` with `query`.
pub async fn redirect(authorization_url: &str, query: &str) -> std::io::Result<String> {
    let params = pairs(authorization_url.split_once('?').unwrap().1);
    let target = params["redirect_uri"]
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let (address, path) = target.split_once('/').unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await?;
    socket
        .write_all(format!("GET /{path}?{query} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
        .await?;
    let mut out = String::new();
    socket.read_to_string(&mut out).await?;
    Ok(out)
}

pub async fn finish_sign_in(sign_in: SignIn, base: &str, seen: &Mutex<Seen>) {
    let url = sign_in.authorization_url().to_owned();
    let params = pairs(url.split_once('?').unwrap().1);
    seen.lock().challenge = Some(params["code_challenge"].clone());
    let finishing = tokio::spawn(sign_in.finish(Duration::from_secs(20)));
    redirect(
        &url,
        &format!("code=code-1&state={}&iss={base}", params["state"]),
    )
    .await
    .unwrap();
    finishing.await.unwrap().unwrap();
}

pub fn http_config(id: &str, url: String) -> McpServerConfig {
    // The fake serves its own metadata: its base is the issuer.
    let issuer = url.trim_end_matches("/mcp").to_owned();
    let mut config = McpServerConfig::http(id, url);
    config.oauth = Some(mcp_bridge::OAuthConfig {
        client_id: Some("praxi-test-client".into()),
        allow_loopback_http: true,
        issuer: Some(issuer),
        ..mcp_bridge::OAuthConfig::default()
    });
    config
}
