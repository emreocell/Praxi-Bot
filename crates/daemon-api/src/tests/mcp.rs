//! `/v1/mcp/*` and `POST /v1/secrets/{name}` against an in-memory `McpControl`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use secret_store::{MemoryStore, SecretStore};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::{T, TOKEN, call, make_chat, send, setup};
use crate::mcp::{
    McpCatalogEntryDto, McpCatalogInstallDto, McpControl, McpControlError, McpServerChanged,
    McpServerDto, McpServerInput, McpStatus, McpToolDto, McpTransportInput, McpTransportKind,
};

struct FakeMcp {
    servers: Mutex<Vec<McpServerDto>>,
    events: broadcast::Sender<McpServerChanged>,
}

impl FakeMcp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            servers: Mutex::new(Vec::new()),
            events: broadcast::channel(16).0,
        })
    }

    fn dto(name: &str, input: &McpServerInput) -> McpServerDto {
        let stdio = matches!(
            input.transport,
            McpTransportInput::Kind(McpTransportKind::Stdio)
        );
        McpServerDto {
            name: name.to_owned(),
            transport: if stdio {
                McpTransportKind::Stdio
            } else {
                McpTransportKind::Http
            },
            command: input.command.clone(),
            args: input.args.clone(),
            url: input.url.clone(),
            env: input.env.clone(),
            cwd: input.cwd.clone(),
            headers: input.headers.clone(),
            tool_allow: input.tool_allow.clone(),
            tool_exclude: input.tool_exclude.clone(),
            secret_env: input.secret_env.clone(),
            secret_headers: input.secret_headers.clone(),
            enabled: input.enabled.unwrap_or(true),
            status: McpStatus::Connected,
            error: None,
            tool_count: 2,
            category: input.category.clone().unwrap_or_else(|| "accounts".into()),
            trust_hints: input.trust_hints,
            bots: input.bots.clone(),
        }
    }

    fn apply_patch(slot: &mut McpServerDto, patch: &Value) -> Result<(), McpControlError> {
        let obj = patch
            .as_object()
            .ok_or_else(|| McpControlError::Invalid("body must be a JSON object".into()))?;
        let present = |k: &str| obj.contains_key(k);
        if present("command") {
            slot.command = obj["command"].as_str().map(str::to_owned);
        }
        if present("args") {
            slot.args = serde_json::from_value(obj["args"].clone()).ok();
        }
        if present("url") {
            slot.url = obj["url"].as_str().map(str::to_owned);
        }
        if present("env") {
            slot.env = serde_json::from_value(obj["env"].clone()).unwrap_or_default();
        }
        if present("cwd") {
            slot.cwd = obj["cwd"].as_str().map(str::to_owned);
        }
        if present("headers") {
            slot.headers = serde_json::from_value(obj["headers"].clone()).unwrap_or_default();
        }
        if present("tool_allow") {
            slot.tool_allow = serde_json::from_value(obj["tool_allow"].clone()).unwrap_or_default();
        }
        if present("tool_exclude") {
            slot.tool_exclude =
                serde_json::from_value(obj["tool_exclude"].clone()).unwrap_or_default();
        }
        if present("secret_env") {
            slot.secret_env = serde_json::from_value(obj["secret_env"].clone()).unwrap_or_default();
        }
        if present("secret_headers") {
            slot.secret_headers =
                serde_json::from_value(obj["secret_headers"].clone()).unwrap_or_default();
        }
        if present("enabled") {
            slot.enabled = obj["enabled"].as_bool().unwrap_or(slot.enabled);
        }
        if present("trust_hints") {
            slot.trust_hints = obj["trust_hints"].as_bool().unwrap_or(slot.trust_hints);
        }
        if present("category")
            && let Some(c) = obj["category"].as_str()
        {
            slot.category = c.to_owned();
        }
        if present("bots") {
            slot.bots = serde_json::from_value(obj["bots"].clone()).unwrap_or(None);
        }
        Ok(())
    }

    fn emit(&self, name: &str, status: McpStatus) {
        let _ = self.events.send(McpServerChanged::new(name, status));
    }
}

#[async_trait]
impl McpControl for FakeMcp {
    fn list(&self) -> Vec<McpServerDto> {
        self.servers.lock().clone()
    }
    async fn add(&self, input: McpServerInput) -> Result<McpServerDto, McpControlError> {
        let name = input
            .name
            .clone()
            .ok_or_else(|| McpControlError::Invalid("name is required".into()))?;
        if input.category.as_deref() == Some("meta") {
            return Err(McpControlError::Invalid("category meta".into()));
        }
        let mut all = self.servers.lock();
        if all.iter().any(|s| s.name == name) {
            return Err(McpControlError::Conflict(format!("{name} exists")));
        }
        let dto = Self::dto(&name, &input);
        all.push(dto.clone());
        drop(all);
        self.emit(&name, McpStatus::Connected);
        Ok(dto)
    }
    async fn update(&self, name: &str, patch: Value) -> Result<McpServerDto, McpControlError> {
        let mut all = self.servers.lock();
        let slot = all
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| McpControlError::NotFound(format!("mcp server {name}")))?;
        Self::apply_patch(slot, &patch)?;
        let dto = slot.clone();
        drop(all);
        self.emit(name, McpStatus::Connected);
        Ok(dto)
    }
    async fn remove(&self, name: &str) -> Result<(), McpControlError> {
        let mut all = self.servers.lock();
        let before = all.len();
        all.retain(|s| s.name != name);
        if all.len() == before {
            return Err(McpControlError::NotFound(format!("mcp server {name}")));
        }
        drop(all);
        self.emit(name, McpStatus::Disconnected);
        Ok(())
    }
    async fn reconnect(&self, name: &str) -> Result<McpServerDto, McpControlError> {
        let all = self.servers.lock();
        let dto = all
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| McpControlError::NotFound(format!("mcp server {name}")))?;
        drop(all);
        self.emit(name, McpStatus::Connecting);
        Ok(dto)
    }
    fn tools(&self, name: &str) -> Result<Vec<McpToolDto>, McpControlError> {
        if !self.servers.lock().iter().any(|s| s.name == name) {
            return Err(McpControlError::NotFound(format!("mcp server {name}")));
        }
        Ok(vec![McpToolDto {
            name: format!("mcp:{name}/echo"),
            description: "Echo".into(),
            effect: "destructive".into(),
        }])
    }
    fn catalog(&self) -> Vec<McpCatalogEntryDto> {
        vec![McpCatalogEntryDto {
            id: "fetch".into(),
            name: "Fetch".into(),
            description: "Fetch pages".into(),
            category: "remote".into(),
            homepage: "https://example.com".into(),
            icon: None,
            install: McpCatalogInstallDto {
                transport: McpTransportKind::Stdio,
                command: Some("uvx".into()),
                args: Some(vec!["mcp-server-fetch".into()]),
                url: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                secret_headers: BTreeMap::new(),
                required_secrets: Vec::new(),
            },
        }]
    }
    fn subscribe(&self) -> broadcast::Receiver<McpServerChanged> {
        self.events.subscribe()
    }
}

fn with_mcp() -> (T, Arc<FakeMcp>) {
    let t = setup();
    let fake = FakeMcp::new();
    t.state.set_mcp(fake.clone());
    (t, fake)
}

fn stdio_body(name: &str) -> Value {
    json!({
        "name": name,
        "transport": "stdio",
        "command": "npx",
        "args": ["-y", "some-server"],
        "secret_env": {"API_KEY": "mcp/some-key"},
    })
}

#[tokio::test]
async fn mcp_routes_need_the_bearer_token() {
    let (t, _) = with_mcp();
    for (m, p) in [
        (Method::GET, "/v1/mcp/servers"),
        (Method::GET, "/v1/mcp/catalog"),
        (Method::DELETE, "/v1/mcp/servers/x"),
        (Method::POST, "/v1/secrets/mcp/k"),
    ] {
        let r = send(&t.app, m, p, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{p}");
    }
}

#[tokio::test]
async fn without_an_embedder_the_routes_answer_503() {
    let t = setup();
    for (m, p) in [
        (Method::GET, "/v1/mcp/servers"),
        (Method::GET, "/v1/mcp/catalog"),
        (Method::GET, "/v1/mcp/servers/x/tools"),
    ] {
        let r = call(&t, m, p, None).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
    }
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/mcp/k",
        Some(json!({"value": "v"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn server_crud_follows_the_contract() {
    let (t, _) = with_mcp();
    let r = call(&t, Method::GET, "/v1/mcp/servers", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!([]));

    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let s = r.json();
    assert_eq!(s["name"], "files");
    assert_eq!(s["transport"], "stdio");
    assert_eq!(s["command"], "npx");
    assert_eq!(s["args"], json!(["-y", "some-server"]));
    assert_eq!(s["status"], "connected");
    assert_eq!(s["tool_count"], 2);
    // 042 defaults, and "all bots" is an explicit null.
    assert_eq!(s["category"], "accounts");
    assert_eq!(s["trust_hints"], false);
    assert!(s.get("bots").is_some() && s["bots"].is_null());
    assert!(s.get("url").is_none() && s.get("error").is_none());
    assert_eq!(s["tool_allow"], json!([]));
    assert_eq!(s["tool_exclude"], json!([]));
    assert_eq!(s["env"], json!({}));
    assert_eq!(s["secret_env"]["API_KEY"], "mcp/some-key");
    assert_eq!(s["secret_headers"], json!({}));
    // The secret name is part of the config; a secret value is not.
    assert!(r.body.windows(12).any(|w| w == b"mcp/some-key"));
    assert!(!r.body.windows(16).any(|w| w == b"ghp_super_secret"));

    // `id` is an alias of `name`; the nested McpServerConfig transport works too.
    let nested = json!({
        "id": "web",
        "transport": {"type": "http", "url": "https://example.com/mcp"},
        "bots": ["a"],
    });
    let r = call(&t, Method::POST, "/v1/mcp/servers", Some(nested)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());

    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "conflict");

    let mut bad = stdio_body("m");
    bad["category"] = json!("meta");
    let r = call(&t, Method::POST, "/v1/mcp/servers", Some(bad)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_mcp_config");

    let r = call(&t, Method::GET, "/v1/mcp/servers", None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 2);

    let mut upd = stdio_body("files");
    upd["trust_hints"] = json!(true);
    upd["bots"] = json!(["bot-1"]);
    upd["tool_allow"] = json!(["read_*"]);
    upd["cwd"] = json!("work");
    let r = call(&t, Method::PUT, "/v1/mcp/servers/files", Some(upd.clone())).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["trust_hints"], true);
    assert_eq!(r.json()["bots"], json!(["bot-1"]));
    assert_eq!(r.json()["tool_allow"], json!(["read_*"]));
    assert_eq!(r.json()["cwd"], "work");
    assert_eq!(r.json()["secret_env"]["API_KEY"], "mcp/some-key");
    let r = call(&t, Method::PUT, "/v1/mcp/servers/nope", Some(upd)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&t, Method::POST, "/v1/mcp/servers/files/reconnect", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["name"], "files");
    let r = call(&t, Method::POST, "/v1/mcp/servers/nope/reconnect", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&t, Method::GET, "/v1/mcp/servers/files/tools", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!([{"name": "mcp:files/echo", "description": "Echo", "effect": "destructive"}])
    );
    let r = call(&t, Method::GET, "/v1/mcp/servers/nope/tools", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&t, Method::DELETE, "/v1/mcp/servers/files", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, "/v1/mcp/servers/files", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_keeps_omitted_fields_and_null_bots_means_all() {
    let (t, _) = with_mcp();
    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let mut upd = stdio_body("files");
    upd["trust_hints"] = json!(true);
    upd["bots"] = json!(["bot-1"]);
    upd["tool_allow"] = json!(["read_*"]);
    upd["cwd"] = json!("work");
    let r = call(&t, Method::PUT, "/v1/mcp/servers/files", Some(upd)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());

    let r = call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/files",
        Some(json!({"trust_hints": false})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let kept = r.json();
    assert_eq!(kept["trust_hints"], false);
    assert_eq!(kept["command"], "npx");
    assert_eq!(kept["args"], json!(["-y", "some-server"]));
    assert_eq!(kept["tool_allow"], json!(["read_*"]));
    assert_eq!(kept["cwd"], "work");
    assert_eq!(kept["secret_env"]["API_KEY"], "mcp/some-key");
    assert_eq!(kept["bots"], json!(["bot-1"]));

    let r = call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/files",
        Some(json!({"bots": null})),
    )
    .await;
    assert!(r.json()["bots"].is_null());
    assert_eq!(r.json()["tool_allow"], json!(["read_*"]));
}

#[tokio::test]
async fn catalog_is_served() {
    let (t, _) = with_mcp();
    let r = call(&t, Method::GET, "/v1/mcp/catalog", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let c = r.json();
    assert_eq!(c[0]["id"], "fetch");
    assert!(c[0]["icon"].is_null());
    assert_eq!(c[0]["install"]["transport"], "stdio");
    assert_eq!(c[0]["install"]["required_secrets"], json!([]));
}

#[tokio::test]
async fn secrets_are_write_only() {
    let t = setup();
    let store = Arc::new(MemoryStore::new());
    t.state.set_secret_store(store.clone());

    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/mcp/github-token",
        Some(json!({"value": "ghp_super_secret"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(r.body.len(), 0);
    let stored = store.get("mcp/github-token").unwrap().unwrap();
    assert_eq!(stored.expose(), "ghp_super_secret");
    // Overwrite.
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/mcp/github-token",
        Some(json!({"value": "second"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        store.get("mcp/github-token").unwrap().unwrap().expose(),
        "second"
    );

    // No way to read it back.
    for m in [Method::GET, Method::PUT, Method::DELETE] {
        let r = call(&t, m, "/v1/secrets/mcp/github-token", None).await;
        assert!(
            matches!(
                r.status,
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
            ),
            "{}",
            r.status
        );
        assert!(!String::from_utf8_lossy(&r.body).contains("second"));
    }
    let r = call(&t, Method::GET, "/v1/secrets", None).await;
    assert!(!String::from_utf8_lossy(&r.body).contains("second"));

    // Bad name / value.
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/bad%20name",
        Some(json!({"value": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/__reserved",
        Some(json!({"value": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/ok",
        Some(json!({"value": ""})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(&t, Method::POST, "/v1/secrets/ok", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(store.list_names().unwrap(), vec!["mcp/github-token"]);
}

#[tokio::test]
async fn mcp_changes_are_announced_on_the_global_stream() {
    let (t, fake) = with_mcp();
    let (_, chat) = make_chat(&t).await;
    let mut body = super::open_global(&t, None).await;

    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let _ = call(&t, Method::POST, "/v1/mcp/servers/files/reconnect", None).await;
    // A background status move (not caused by a route) is announced too.
    fake.emit("files", McpStatus::Error);
    let r = call(&t, Method::DELETE, "/v1/mcp/servers/files", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    let run = super::send_msg(&t, &chat, "after", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fr = super::read_until(&mut body, |fr| super::run_finished_of(fr, &run)).await;
    let seen: Vec<(String, String)> = fr
        .iter()
        .filter(|f| f.1 == "mcp_server_changed")
        .map(|f| {
            assert_eq!(f.0, "", "no SSE id");
            assert_eq!(f.2["type"], "mcp_server_changed");
            (
                f.2["name"].as_str().unwrap().to_owned(),
                f.2["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let statuses: Vec<&str> = seen.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(
        statuses,
        ["connected", "connecting", "error", "disconnected"]
    );
    assert!(seen.iter().all(|s| s.0 == "files"));
    let _ = TOKEN;
}
