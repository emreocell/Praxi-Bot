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
            oauth: input.oauth.clone(),
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
            error_kind: None,
            stderr_tail: Vec::new(),
            tool_count: 2,
            category: input.category.clone().unwrap_or_else(|| "accounts".into()),
            trust_hints: input.trust_hints,
            public_content: input.public_content,
            local_only: input.local_only,
            browser: input.browser,
            is_browser: input.browser,
            bots: input.bots.clone(),
            changed_tools: Vec::new(),
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
        if present("public_content") {
            slot.public_content = obj["public_content"]
                .as_bool()
                .unwrap_or(slot.public_content);
        }
        if present("local_only") {
            slot.local_only = obj["local_only"].as_bool().unwrap_or(slot.local_only);
        }
        if present("category")
            && let Some(c) = obj["category"].as_str()
        {
            slot.category = c.to_owned();
        }
        if present("bots") {
            slot.bots = serde_json::from_value(obj["bots"].clone()).unwrap_or(None);
        }
        if let (Some(list), Some(add)) = (slot.bots.as_mut(), obj.get("bots_add")) {
            for id in serde_json::from_value::<Vec<String>>(add.clone()).unwrap_or_default() {
                if !list.contains(&id) {
                    list.push(id);
                }
            }
        }
        if let (Some(list), Some(rm)) = (slot.bots.as_mut(), obj.get("bots_remove")) {
            let rm = serde_json::from_value::<Vec<String>>(rm.clone()).unwrap_or_default();
            list.retain(|id| !rm.contains(id));
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
    async fn approve_tool_changes(
        &self,
        name: &str,
        _tool: &str,
        hash: &str,
    ) -> Result<McpServerDto, McpControlError> {
        if hash == "stale" {
            return Err(McpControlError::ToolChangedAgain(
                "tool changed again".into(),
            ));
        }
        self.servers
            .lock()
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| McpControlError::NotFound(format!("mcp server {name}")))
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
                folder: None,
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
    assert_eq!(s["public_content"], false);
    assert_eq!(s["local_only"], false);
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
async fn deleting_a_bot_removes_its_id_from_every_server_and_never_widens_access() {
    let (t, _) = with_mcp();
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": super::PROFILE })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let bot = r.json()["id"].as_str().unwrap().to_owned();

    for (name, bots) in [
        ("shared", json!([bot, "other-bot"])),
        ("only-me", json!([bot])),
        ("others", json!(["other-bot"])),
        ("open", Value::Null),
    ] {
        let mut body = stdio_body(name);
        body["bots"] = bots;
        let r = call(&t, Method::POST, "/v1/mcp/servers", Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    }

    let r = call(&t, Method::DELETE, &format!("/v1/bots/{bot}"), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    let list = call(&t, Method::GET, "/v1/mcp/servers", None).await.json();
    let bots_of = |n: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .unwrap()["bots"]
            .clone()
    };
    assert_eq!(bots_of("shared"), json!(["other-bot"]));
    // Emptied list stays an explicit empty list ("no bot"), never null ("all").
    assert_eq!(bots_of("only-me"), json!([]));
    assert_eq!(bots_of("others"), json!(["other-bot"]));
    assert!(bots_of("open").is_null());
}

#[tokio::test]
async fn failed_bot_delete_keeps_the_server_lists() {
    let (t, _) = with_mcp();
    let (bot, _chat) = make_chat(&t).await;
    let mut body = stdio_body("mine");
    body["bots"] = json!([bot]);
    let r = call(&t, Method::POST, "/v1/mcp/servers", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = call(&t, Method::DELETE, &format!("/v1/bots/{bot}"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let list = call(&t, Method::GET, "/v1/mcp/servers", None).await.json();
    assert_eq!(list[0]["bots"], json!([bot]));
}

#[tokio::test]
async fn empty_bots_list_round_trips_and_differs_from_null() {
    let (t, _) = with_mcp();
    let mut body = stdio_body("nobody");
    body["bots"] = json!([]);
    let r = call(&t, Method::POST, "/v1/mcp/servers", Some(body)).await;
    assert_eq!(r.json()["bots"], json!([]));
    let r = call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/nobody",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(r.json()["bots"], json!([]));
}

#[tokio::test]
async fn oversized_or_malformed_bot_id_lists_are_rejected_with_400() {
    let (t, _) = with_mcp();
    let long = "x".repeat(65);
    let many: Vec<String> = (0..257).map(|i| format!("b{i}")).collect();
    let mut body = stdio_body("ok");
    body["bots"] = json!(["fine"]);
    assert_eq!(
        call(&t, Method::POST, "/v1/mcp/servers", Some(body))
            .await
            .status,
        StatusCode::CREATED
    );
    for bad in [json!([long]), json!([""]), json!(many)] {
        let mut b = stdio_body("bad");
        b["bots"] = bad.clone();
        let r = call(&t, Method::POST, "/v1/mcp/servers", Some(b)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "create {bad}");
        for field in ["bots", "bots_add", "bots_remove"] {
            let r = call(
                &t,
                Method::PUT,
                "/v1/mcp/servers/ok",
                Some(json!({ field: bad })),
            )
            .await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{field} {bad}");
        }
    }
    // The stored list is untouched by the rejected requests.
    let list = call(&t, Method::GET, "/v1/mcp/servers", None).await.json();
    assert_eq!(list[0]["bots"], json!(["fine"]));
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

#[tokio::test]
async fn oauth_routes_are_host_local_for_every_peer_role() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let (t, _) = with_mcp();
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, suffix) in [
            (Method::POST, "start"),
            (Method::POST, "cancel"),
            (Method::POST, "callback"),
            (Method::POST, "logout"),
            (Method::GET, "status"),
        ] {
            for path in [
                format!("/v1/mcp/servers/s/auth/{suffix}"),
                format!("/v1/mcp/servers/s/auth/{suffix}/"),
                format!("/v1/mcp//servers/s//auth/{suffix}"),
                format!("/v1/mcp/servers/s/%61uth%2F{suffix}"),
            ] {
                let mut request = Request::builder()
                    .method(method.clone())
                    .uri(&path)
                    .body(Body::empty())
                    .unwrap();
                request.extensions_mut().insert(crate::PeerIdentity {
                    key: "oauth-peer".into(),
                    role,
                });
                let response = crate::peer_router(t.state.clone())
                    .oneshot(request)
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{role:?} {path}");
            }
        }
    }
    // Local authentication is still mandatory and user requests reach the handler.
    for (method, suffix) in [
        (Method::POST, "start"),
        (Method::POST, "cancel"),
        (Method::POST, "logout"),
        (Method::GET, "status"),
    ] {
        let path = format!("/v1/mcp/servers/s/auth/{suffix}");
        let r = send(&t.app, method.clone(), &path, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            call(&t, method, &path, None).await.status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let path = "/v1/mcp/servers/s/auth/callback";
    let body = json!({"redirect_url": "http://127.0.0.1:1/callback?code=c&state=s"});
    let r = send(&t.app, Method::POST, path, Some(body.clone()), &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&t, Method::POST, path, Some(body)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Review finding 1b: MCP configuration decides which secrets go where and
/// which commands run on this host, so no paired role may change it.
#[tokio::test]
async fn mcp_management_is_host_local_for_every_peer_role() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let (t, fake) = with_mcp();
    let exfil = json!({
        "name": "attacker", "transport": "http", "url": "https://attacker.example/mcp",
        "secret_headers": {"X-Stolen": "mcp/victim/oauth"},
    });
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, path) in [
            (Method::POST, "/v1/mcp/servers"),
            (Method::PUT, "/v1/mcp/servers/s"),
            (Method::DELETE, "/v1/mcp/servers/s"),
            (Method::POST, "/v1/mcp/servers/s/reconnect"),
            (Method::POST, "/v1/mcp/servers/s/approve-tool-changes"),
        ] {
            let variants = [
                path.to_owned(),
                format!("{path}/"),
                path.replacen("/v1/mcp/", "/v1//mcp//", 1),
                path.replacen("servers", "%73ervers", 1),
                path.replacen("/v1/mcp/servers", "/v1/mcp%2Fservers", 1),
            ];
            for uri in variants {
                let mut request = Request::builder()
                    .method(method.clone())
                    .uri(&uri)
                    .header("content-type", "application/json")
                    .body(Body::from(exfil.to_string()))
                    .unwrap();
                request.extensions_mut().insert(crate::PeerIdentity {
                    key: "mcp-peer".into(),
                    role,
                });
                let response = crate::peer_router(t.state.clone())
                    .oneshot(request)
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {uri}"
                );
            }
        }
    }
    assert!(
        fake.servers.lock().is_empty(),
        "a peer request reached the host"
    );
    // Reading stays allowed for the roles that could read before.
    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/v1/mcp/servers")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(crate::PeerIdentity {
        key: "mcp-peer".into(),
        role: crate::PeerRole::Desktop,
    });
    let response = crate::peer_router(t.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Review finding 1a: an OAuth record can never be planted through the
/// generic secrets route, for any caller.
#[tokio::test]
async fn managed_oauth_records_cannot_be_written_through_the_secrets_route() {
    let (t, _) = with_mcp();
    let store = Arc::new(MemoryStore::new());
    t.state.set_secret_store(store.clone());
    for name in [
        "mcp/victim/oauth",
        "MCP/Victim/OAuth",
        "mcp/victim/oauth-client",
    ] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/secrets/{name}"),
            Some(json!({"value": "{\"resource\":\"https://attacker.example/mcp\"}"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{name}");
    }
    assert_eq!(store.list_names().unwrap().len(), 0);
}

/// Review finding 1c: every MCP mutation writes an audit row before it runs;
/// rows carry the actor and names only, never a config value.
#[tokio::test]
async fn every_mcp_mutation_is_audited_without_values() {
    let (t, _) = with_mcp();
    t.state.set_secret_store(Arc::new(MemoryStore::new()));
    let mut body = stdio_body("files");
    body["env"] = json!({"PLAIN": "plain-env-value-31f"});
    let calls = [
        (
            Method::POST,
            "/v1/mcp/servers".to_owned(),
            Some(body),
            "mcp.server_create",
        ),
        (
            Method::PUT,
            "/v1/mcp/servers/files".to_owned(),
            Some(json!({"enabled": false})),
            "mcp.server_update",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/reconnect".to_owned(),
            None,
            "mcp.server_reconnect",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/approve-tool-changes".to_owned(),
            Some(json!({"tool": "echo", "hash": "abc"})),
            "mcp.tool_changes_approved",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/auth/start".to_owned(),
            None,
            "mcp.auth_start",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/auth/callback".to_owned(),
            Some(json!({"redirect_url": "http://127.0.0.1:1/callback?code=code-value-9e&state=s"})),
            "mcp.auth_callback",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/auth/cancel".to_owned(),
            None,
            "mcp.auth_cancel",
        ),
        (
            Method::POST,
            "/v1/mcp/servers/files/auth/logout".to_owned(),
            None,
            "mcp.auth_logout",
        ),
        (
            Method::POST,
            "/v1/secrets/mcp_files_key".to_owned(),
            Some(json!({"value": "secret-value-77d"})),
            "secret.set",
        ),
        (
            Method::DELETE,
            "/v1/mcp/servers/files".to_owned(),
            None,
            "mcp.server_delete",
        ),
    ];
    for (method, path, body, action) in calls {
        let before = t
            .state
            .storage()
            .list_audit(t.state.user_id())
            .unwrap()
            .len();
        let _ = call(&t, method, &path, body).await;
        let rows = t.state.storage().list_audit(t.state.user_id()).unwrap();
        let mine: Vec<_> = rows[before..]
            .iter()
            .filter(|r| r.action == action)
            .collect();
        assert_eq!(mine.len(), 2, "{action}: {rows:?}");
        assert_eq!(mine[0].detail["phase"], "requested", "{action}");
        assert_eq!(mine[1].detail["phase"], "completed", "{action}");
    }
    let all = format!(
        "{:?}",
        t.state.storage().list_audit(t.state.user_id()).unwrap()
    );
    for value in ["plain-env-value-31f", "code-value-9e", "secret-value-77d"] {
        assert!(!all.contains(value), "{value} in audit");
    }
}

/// Believing a server's own tool labels is a security choice: every change of
/// `trust_hints` (add or edit) leaves its own audit row with the new value.
#[tokio::test]
async fn changing_label_trust_is_audited_with_the_new_value() {
    let (t, _) = with_mcp();
    let rows = |action: &str| {
        t.state
            .storage()
            .list_audit(t.state.user_id())
            .unwrap()
            .into_iter()
            .filter(|r| r.action == action)
            .map(|r| (r.detail["name"].clone(), r.detail["trust_hints"].clone()))
            .collect::<Vec<_>>()
    };
    // Added without trust: the safe default writes no trust row.
    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    assert_eq!(rows("mcp.trust_hints_changed").len(), 0);
    let r = call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/files",
        Some(json!({"trust_hints": true})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let r = call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/files",
        Some(json!({"trust_hints": false})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    // An unrelated edit writes no trust row.
    call(
        &t,
        Method::PUT,
        "/v1/mcp/servers/files",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(
        rows("mcp.trust_hints_changed"),
        [
            (json!("files"), json!(true)),
            (json!("files"), json!(false))
        ]
    );
}

/// 244-A review finding 1: MCP management starts programs and names host
/// secrets, so every mutation is refused to every paired peer role (also under
/// encoded, double-slash and trailing-slash spellings); GET stays open.
#[tokio::test]
async fn peer_router_refuses_every_mcp_mutation_to_every_role() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt as _;
    let (t, fake) = with_mcp();
    let body = stdio_body("files").to_string();
    let routes: Vec<(Method, String)> = vec![
        (Method::POST, "/v1/mcp/servers".into()),
        (Method::POST, "/v1/mcp/servers/".into()),
        (Method::POST, "//v1//mcp//servers".into()),
        (Method::POST, "/v1/mcp%2Fservers".into()),
        (Method::PUT, "/v1/mcp/servers/files".into()),
        (Method::PUT, "/v1/mcp/servers/files/".into()),
        (Method::PUT, "/v1/mcp/servers%2Ffiles".into()),
        (Method::PATCH, "/v1/mcp/servers/files".into()),
        (Method::DELETE, "/v1/mcp/servers/files".into()),
        (Method::DELETE, "/v1/mcp//servers/files".into()),
        (Method::POST, "/v1/mcp/servers/files/reconnect".into()),
        (Method::POST, "/v1/mcp/servers/files/reconnect/".into()),
        (Method::POST, "/v1/mcp/servers//files/reconnect".into()),
        (Method::POST, "/v1/mcp%2Fservers%2Ffiles%2Freconnect".into()),
        (Method::POST, "/v1/mcp/anything-new".into()),
    ];
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, path) in &routes {
            let mut req = Request::builder()
                .method(method.clone())
                .uri(path.as_str())
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap();
            req.extensions_mut().insert(crate::PeerIdentity {
                key: "peer-key".into(),
                role,
            });
            let resp = crate::peer_router(t.state.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "role={role:?} {method} {path} should be 403"
            );
        }
        // Reading stays as it is today.
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("/v1/mcp/servers")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(crate::PeerIdentity {
            key: "peer-key".into(),
            role,
        });
        let resp = crate::peer_router(t.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        // Unchanged by this rule: desktop and phone peers read, node and host
        // peers were already refused every non-desktop/phone route.
        let want = if matches!(role, crate::PeerRole::Desktop | crate::PeerRole::Phone) {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(resp.status(), want, "GET for {role:?}");
    }
    // An encoded `/` in a server-name segment is refused even for GET.
    for role in [crate::PeerRole::Desktop, crate::PeerRole::Phone] {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("/v1/mcp/servers/a%2Fb/tools")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(crate::PeerIdentity {
            key: "peer-key".into(),
            role,
        });
        let resp = crate::peer_router(t.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "%2F for {role:?}");
    }
    assert!(fake.list().is_empty(), "a peer must not add a server");
}

#[tokio::test]
async fn approving_changed_tools_reaches_the_host() {
    let (t, _) = with_mcp();
    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers",
        Some(stdio_body("files")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let body = Some(json!({"tool": "echo", "hash": "abc"}));
    let path = "/v1/mcp/servers/files/approve-tool-changes";
    let r = call(&t, Method::POST, path, body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    // The hash is required, and a stale one is a typed conflict.
    let r = call(&t, Method::POST, path, Some(json!({"tool": "echo"}))).await;
    assert!(r.status.is_client_error());
    let r = call(
        &t,
        Method::POST,
        path,
        Some(json!({"tool": "echo", "hash": "stale"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "mcp_tool_changed_again");
    let r = call(
        &t,
        Method::POST,
        "/v1/mcp/servers/nope/approve-tool-changes",
        body,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// `POST /v1/connection-test/mcp`: owner-only, typed outcome, no tool is run.
#[tokio::test]
async fn connection_test_mcp_maps_state_to_typed_outcome() {
    use super::{OWNER_KEY, call_token_only};
    use crate::mcp::McpErrorKind;
    let _ = OWNER_KEY;
    let (t, fake) = with_mcp();
    let input: McpServerInput = serde_json::from_value(stdio_body("demo")).unwrap();
    fake.servers.lock().push(FakeMcp::dto("demo", &input));
    let body = || Some(json!({"name": "demo"}));
    let path = "/v1/connection-test/mcp";

    // The token alone cannot start a server process.
    let r = call_token_only(&t, Method::POST, path, body()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = call(&t, Method::POST, path, body()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    assert_eq!(r.json()["outcome"], "ok");

    {
        let mut all = fake.servers.lock();
        all[0].status = McpStatus::Error;
        all[0].error = Some("failed to start `npx`".into());
        all[0].error_kind = Some(McpErrorKind::RuntimeMissing);
    }
    let r = call(&t, Method::POST, path, body()).await;
    assert_eq!(r.json()["outcome"], "wrong_endpoint");
    assert!(r.json()["details"].as_str().unwrap().contains("npx"));

    {
        let mut all = fake.servers.lock();
        all[0].error_kind = Some(McpErrorKind::Timeout);
    }
    let r = call(&t, Method::POST, path, body()).await;
    assert_eq!(r.json()["outcome"], "no_network");
}
