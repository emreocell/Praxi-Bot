//! Client-compatibility tests: raw newline-delimited JSON-RPC over a pipe, the
//! way real MCP clients (Claude Desktop/Code, Cursor, VS Code, Zed) speak to a
//! stdio server. Expected shapes come from the MCP specification revisions
//! 2024-11-05, 2025-03-26, 2025-06-18 and 2025-11-25 (the versions rmcp 3.9
//! negotiates) and from JSON-RPC 2.0. No network, desktop or account is used;
//! the daemon is a wiremock fake.
//!
//! Deviations that live inside rmcp (not fixable here without wrapping the
//! transport) are tolerated explicitly below: invalid JSON gets no `-32700`
//! reply, a batch frame gets `-32600` without `"id": null`, and `tools/call`
//! without `name` answers `-32601` instead of `-32602`.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const TOKEN: &str = "praxi-mcp.00000000-0000-0000-0000-000000000001.compat-test-credential";
const TOOL: &str = "task_start";
const VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

struct Peer {
    lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
    _daemon: MockServer,
}

impl Peer {
    async fn new(call: Option<ResponseTemplate>) -> Self {
        let daemon = MockServer::start().await;
        let tools = json!([
            {"name":TOOL,"description":"Start a task","read_only":false,"untrusted_output":true,
             "input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},
            {"name":"task_status","description":"Read status","read_only":true,"untrusted_output":true,
             "input_schema":{"type":"object","properties":{"run":{"type":"string"}}}}
        ]);
        Mock::given(method("GET"))
            .and(path("/v1/mcp-server/tools"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tools))
            .mount(&daemon)
            .await;
        if let Some(template) = call {
            Mock::given(method("POST"))
                .and(path("/v1/mcp-server/call"))
                .respond_with(template)
                .mount(&daemon)
                .await;
        }
        let server = McpServer::connect(
            Client::scoped(&daemon.uri(), TOKEN.into()).unwrap(),
            Uuid::now_v7(),
            &BTreeSet::from([McpScope::Tasks]),
        )
        .await
        .unwrap();
        let (server_pipe, client_pipe) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(server_pipe);
        tokio::spawn(async move {
            if let Ok(service) = server.serve((FrameLimit::new(read), write)).await {
                let _ = service.waiting().await;
            }
        });
        let (read, write) = tokio::io::split(client_pipe);
        Self {
            lines: BufReader::new(read).lines(),
            write,
            _daemon: daemon,
        }
    }

    async fn send_raw(&mut self, line: &str) {
        self.write.write_all(line.as_bytes()).await.unwrap();
        self.write.write_all(b"\n").await.unwrap();
    }

    /// Next frame, or `None` on EOF / silence.
    async fn recv(&mut self, wait: Duration) -> Option<Value> {
        let line = tokio::time::timeout(wait, self.lines.next_line())
            .await
            .ok()?
            .ok()??;
        Some(serde_json::from_str(&line).expect("server frames are single-line JSON"))
    }

    async fn request(&mut self, value: Value) -> Value {
        self.send_raw(&value.to_string()).await;
        self.recv(Duration::from_secs(5))
            .await
            .expect("server must answer a request")
    }

    async fn initialize(&mut self, version: &str) -> Value {
        let reply = self
            .request(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":version,"capabilities":{},
                "clientInfo":{"name":"compat-test","version":"0"}}}),
            )
            .await;
        self.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        reply
    }

    async fn ready(call: Option<ResponseTemplate>) -> Self {
        let mut peer = Self::new(call).await;
        peer.initialize("2025-06-18").await;
        peer
    }

    async fn ping(&mut self, id: i64) {
        let reply = self
            .request(json!({"jsonrpc":"2.0","id":id,"method":"ping"}))
            .await;
        assert_eq!(reply, json!({"jsonrpc":"2.0","id":id,"result":{}}));
    }
}

fn call_request(id: i64, name: &str, args: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})
}

#[tokio::test]
async fn initialize_negotiates_protocol_versions_and_capabilities() {
    for requested in VERSIONS {
        let mut peer = Peer::new(None).await;
        let reply = peer.initialize(requested).await;
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 1);
        let result = &reply["result"];
        // A server answers with a revision it supports, never newer than asked.
        let negotiated = result["protocolVersion"].as_str().unwrap();
        assert!(
            VERSIONS.contains(&negotiated) && negotiated <= requested,
            "{requested} negotiated {negotiated}"
        );
        // Only tools are offered: no resources, prompts, logging or sampling.
        let capabilities = result["capabilities"].as_object().unwrap();
        assert_eq!(capabilities.keys().collect::<Vec<_>>(), ["tools"]);
        assert_eq!(result["serverInfo"]["name"], "praxi");
        assert!(result["serverInfo"]["version"].is_string());
    }
    // A client from the future gets the newest revision the server speaks.
    let mut peer = Peer::new(None).await;
    let reply = peer.initialize("2099-01-01").await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn tools_list_has_valid_schemas_annotations_and_rejects_unknown_cursors() {
    let mut peer = Peer::ready(None).await;
    let reply = peer
        .request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    // Single page: spec 2025-06-18 "Pagination" says a missing nextCursor
    // means the end of the list.
    assert!(reply["result"].get("nextCursor").is_none());
    for tool in tools {
        assert_ne!(tool["name"].as_str().unwrap(), "");
        assert_ne!(tool["description"].as_str().unwrap(), "");
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["annotations"]["readOnlyHint"].is_boolean());
    }
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], true);
    // The server never hands out a cursor, so one is invalid params (-32602)
    // rather than silently returning the whole list again.
    let reply = peer
        .request(json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"cursor":"x"}}))
        .await;
    assert_eq!(reply["error"]["code"], -32602);
    // An empty params object is an ordinary first-page request.
    let reply = peer
        .request(json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}))
        .await;
    assert_eq!(reply["result"]["tools"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn tools_call_success_and_error_shapes() {
    let mut peer = Peer::ready(Some(
        ResponseTemplate::new(200).set_body_json(json!({"run":"r-1"})),
    ))
    .await;
    let ok = peer
        .request(call_request(2, TOOL, &json!({"text":"hi"})))
        .await;
    assert_ne!(ok["result"]["isError"], true);
    assert_eq!(ok["result"]["content"][0]["type"], "text");
    let text = ok["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("[Praxi tool result: UNTRUSTED"));
    assert!(text.contains("r-1"));
    // An unknown tool is a tool-level error (isError), not a protocol error,
    // so the model can see it (spec 2025-06-18, tools "Error Handling").
    let unknown = peer
        .request(call_request(3, "browser_eval", &json!({})))
        .await;
    assert!(unknown.get("error").is_none());
    assert_eq!(unknown["result"]["isError"], true);
    assert_eq!(unknown["result"]["content"][0]["type"], "text");
    // A missing `name` is a malformed request: an error object, never a crash.
    let bad = peer
        .request(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}))
        .await;
    assert!(bad["error"]["code"].is_i64());
    assert!(bad["error"]["message"].is_string());
    peer.ping(5).await;
}

#[tokio::test]
async fn daemon_failures_become_is_error_results() {
    for template in [
        ResponseTemplate::new(429).set_body_json(json!({"error":"rate limited"})),
        ResponseTemplate::new(401).set_body_json(json!({"error":"expired"})),
        ResponseTemplate::new(200).set_body_json(json!({"is_error":true,"text":"refused"})),
    ] {
        let mut peer = Peer::ready(Some(template)).await;
        let reply = peer
            .request(call_request(2, TOOL, &json!({"text":"x"})))
            .await;
        assert!(reply.get("error").is_none(), "{reply}");
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert!(reply["result"]["content"][0]["text"].is_string());
        peer.ping(3).await;
    }
}

#[tokio::test]
async fn ping_unknown_methods_and_notifications() {
    let mut peer = Peer::ready(None).await;
    // CRLF framing (some Windows clients) must be tolerated.
    peer.send_raw("{\"jsonrpc\":\"2.0\",\"id\":\"s-1\",\"method\":\"ping\"}\r")
        .await;
    let reply = peer.recv(Duration::from_secs(5)).await.unwrap();
    assert_eq!(reply, json!({"jsonrpc":"2.0","id":"s-1","result":{}}));
    // Unknown method => -32601 with the string id echoed.
    let reply = peer
        .request(json!({"jsonrpc":"2.0","id":"m-1","method":"does/not/exist"}))
        .await;
    assert_eq!(reply["id"], "m-1");
    assert_eq!(reply["error"]["code"], -32601);
    // A method of a capability the server did not advertise.
    let reply = peer
        .request(
            json!({"jsonrpc":"2.0","id":3,"method":"logging/setLevel","params":{"level":"info"}}),
        )
        .await;
    assert_eq!(reply["error"]["code"], -32601);
    // Notifications never get a response, including unknown ones and a
    // cancellation for a request that does not exist.
    for notification in [
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":404}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/unknown"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}"#,
    ] {
        peer.send_raw(notification).await;
    }
    assert!(peer.recv(Duration::from_millis(300)).await.is_none());
    peer.ping(4).await;
}

#[tokio::test]
async fn malformed_frames_do_not_kill_the_session() {
    let mut peer = Peer::ready(None).await;
    // Invalid JSON: JSON-RPC asks for -32700; rmcp drops the frame silently.
    // Either is tolerated, but the session must stay usable.
    peer.send_raw("{not json").await;
    if let Some(reply) = peer.recv(Duration::from_millis(300)).await {
        assert_eq!(reply["error"]["code"], -32700);
    }
    // MCP 2025-06-18 removed JSON-RPC batching; a batch must never execute.
    peer.send_raw(r#"[{"jsonrpc":"2.0","id":7,"method":"tools/list"}]"#)
        .await;
    if let Some(reply) = peer.recv(Duration::from_millis(500)).await {
        assert_eq!(reply["error"]["code"], -32600);
        assert!(reply.get("result").is_none());
    }
    // Missing "jsonrpc" member.
    peer.send_raw(r#"{"id":8,"method":"ping"}"#).await;
    let _ = peer.recv(Duration::from_millis(300)).await;
    peer.ping(9).await;
}

#[tokio::test]
async fn oversized_frame_closes_the_transport_without_a_result() {
    let mut peer = Peer::ready(None).await;
    let huge = call_request(2, TOOL, &json!({"text":"x".repeat(70 * 1024)}));
    // The server may close its end mid-write; ignore the write error.
    let _ = peer.write.write_all(huge.to_string().as_bytes()).await;
    let _ = peer.write.write_all(b"\n").await;
    while let Some(frame) = peer.recv(Duration::from_secs(3)).await {
        assert!(frame.get("result").is_none(), "{frame}");
    }
}

#[tokio::test]
async fn cancelled_notification_silences_a_pending_call_and_keeps_the_session() {
    let mut peer = Peer::ready(Some(
        ResponseTemplate::new(200)
            .set_body_json(json!({"run":"late"}))
            .set_delay(Duration::from_secs(30)),
    ))
    .await;
    peer.send_raw(&call_request(42, TOOL, &json!({"text":"slow"})).to_string())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    peer.send_raw(
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42,"reason":"user"}}"#,
    )
    .await;
    // Spec 2025-06-18 (cancellation): the receiver stops work and SHOULD NOT
    // answer the cancelled request. The daemon-side effect (HTTP request
    // dropped, approval cancelled) is covered by tests/mcp_server_cancellation.rs.
    while let Some(frame) = peer.recv(Duration::from_millis(700)).await {
        assert_ne!(frame["id"], 42, "cancelled request was answered: {frame}");
    }
    peer.ping(43).await;
}
