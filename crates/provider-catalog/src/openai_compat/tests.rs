#![allow(clippy::assert_is_empty)]
use std::time::Duration;

use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

use super::{OpenAiCompatConfig, OpenAiCompatProvider, request::build_body, sse::SseDecoder};
use crate::{
    entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind},
    error::ProviderError,
    provider::{
        ChatChunk, ChatProvider, ChatRequest, FinishReason, Message, MessageRole, ModelRef,
        ToolCall, ToolChoice, Usage,
    },
};

fn entry(base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: "mock".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: base_url.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: true,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: Some(format!("{base_url}/models")),
    }
}

fn fast() -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        max_retries: 2,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        ..OpenAiCompatConfig::default()
    }
}

fn req() -> ChatRequest {
    ChatRequest::new(
        ModelRef("mock/some-model".into()),
        vec![Message::text(MessageRole::User, "hi")],
    )
}

fn sse(events: &[&str]) -> ResponseTemplate {
    let mut body = String::new();
    for e in events {
        body.push_str("data: ");
        body.push_str(e);
        body.push_str("\n\n");
    }
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn collect(
    p: &OpenAiCompatProvider,
    r: ChatRequest,
) -> Result<Vec<ChatChunk>, ProviderError> {
    p.stream(r).await?.try_collect().await
}

fn provider(server: &MockServer) -> OpenAiCompatProvider {
    OpenAiCompatProvider::with_api_key(entry(&server.uri()), fast(), Some("sk-secret".into()))
        .unwrap()
}

#[tokio::test]
async fn plain_text_stream_with_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer sk-secret"))
        .and(body_partial_json(
            json!({"model": "some-model", "stream": true}),
        ))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}"#,
            "[DONE]",
        ]))
        .expect(1)
        .mount(&server)
        .await;

    let chunks = collect(&provider(&server), req()).await.unwrap();
    let text: String = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    assert!(chunks.iter().any(|c| matches!(
        c,
        ChatChunk::Usage(Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            cached_tokens: Some(4)
        })
    )));
    assert!(matches!(
        chunks.last(),
        Some(ChatChunk::Done {
            finish_reason: FinishReason::Stop
        })
    ));
}

#[tokio::test]
async fn multiple_tool_calls_split_across_chunks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"list_dir","arguments":"{\"p"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"ath\":\".\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]))
        .mount(&server)
        .await;

    let chunks = collect(&provider(&server), req()).await.unwrap();
    let calls: Vec<&ToolCall> = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::ToolCall(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
    assert_eq!(calls[1].id, "call_b");
    assert_eq!(calls[1].arguments, r#"{"path":"."}"#);
    assert!(matches!(
        chunks.last(),
        Some(ChatChunk::Done {
            finish_reason: FinishReason::ToolCalls
        })
    ));
}

#[tokio::test]
async fn tool_call_without_id_gets_synthetic_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server), req()).await.unwrap();
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, ChatChunk::ToolCall(t) if t.id == "call_0" && t.name == "f"))
    );
}

#[tokio::test]
async fn retries_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server), req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "ok"));
}

#[tokio::test]
async fn rate_limit_exhausts_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .expect(3) // 1 attempt + 2 retries
        .mount(&server)
        .await;
    let err = provider(&server).stream(req()).await.err().unwrap();
    assert!(matches!(
        err,
        ProviderError::RateLimited {
            retry_after: Some(d)
        } if d == Duration::ZERO
    ));
}

#[tokio::test]
async fn server_error_is_retried_and_mapped_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":{"message":"down"}})))
        .expect(3)
        .mount(&server)
        .await;
    let err = provider(&server).stream(req()).await.err().unwrap();
    assert!(
        matches!(err, ProviderError::Upstream { status: 503, ref message } if message == "down")
    );
}

#[tokio::test]
async fn auth_error_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":{"message":"bad key"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server).stream(req()).await.err().unwrap();
    assert!(matches!(err, ProviderError::Auth(ref m) if m == "bad key"));
}

#[tokio::test]
async fn other_4xx_maps_to_api_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server).stream(req()).await.err().unwrap();
    assert!(matches!(err, ProviderError::Api { status: 400, .. }));
}

#[tokio::test]
async fn timeout_is_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(400)))
        .mount(&server)
        .await;
    let cfg = OpenAiCompatConfig {
        timeout: Duration::from_millis(50),
        ..fast()
    };
    let p = OpenAiCompatProvider::with_api_key(entry(&server.uri()), cfg, None).unwrap();
    let err = p.stream(req()).await.err().unwrap();
    assert!(matches!(err, ProviderError::Timeout));
}

#[tokio::test]
async fn non_streaming_fallback_maps_full_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"stream": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "hello",
                    "tool_calls": [{"id": "c1", "type": "function",
                        "function": {"name": "t", "arguments": "{\"x\":1}"}}]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut e = entry(&server.uri());
    e.capabilities.streaming = false;
    let p = OpenAiCompatProvider::with_api_key(e, fast(), None).unwrap();
    let chunks = collect(&p, req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "hello"));
    assert!(
        matches!(&chunks[1], ChatChunk::ToolCall(t) if t.id == "c1" && t.arguments == "{\"x\":1}")
    );
    assert!(matches!(
        &chunks[2],
        ChatChunk::Usage(Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
            cached_tokens: None
        })
    ));
    assert!(matches!(
        &chunks[3],
        ChatChunk::Done {
            finish_reason: FinishReason::ToolCalls
        }
    ));
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[r#"{"choices":[{"delta":{"content":"par"}}]}"#]))
        .mount(&server)
        .await;
    let err = collect(&provider(&server), req()).await.unwrap_err();
    assert!(matches!(err, ProviderError::Stream(_)));
}

#[tokio::test]
async fn in_stream_error_event_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[r#"{"error":{"message":"overloaded"}}"#]))
        .mount(&server)
        .await;
    let err = collect(&provider(&server), req()).await.unwrap_err();
    assert!(matches!(err, ProviderError::Stream(ref m) if m.contains("overloaded")));
}

#[tokio::test]
async fn list_models_parses_data_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "a"}, {"id": "b/c"}]
        })))
        .mount(&server)
        .await;
    let models = provider(&server).list_models().await.unwrap();
    assert_eq!(models, vec!["a", "b/c"]);
}

#[tokio::test]
async fn list_models_without_endpoint_is_empty() {
    let mut e = entry("http://127.0.0.1:1");
    e.models_endpoint = None;
    let p = OpenAiCompatProvider::with_api_key(e, fast(), None).unwrap();
    assert!(p.list_models().await.unwrap().is_empty());
}

#[test]
fn sse_decoder_handles_split_bytes_crlf_and_comments() {
    let mut d = SseDecoder::default();
    assert!(d.push(b": keepalive\r\nda").is_empty());
    assert!(d.push(b"ta: {\"a\":1}\r").is_empty());
    let ev = d.push(b"\n\r\ndata:[DONE]\n\n");
    assert_eq!(ev, vec![r#"{"a":1}"#, "[DONE]"]);
    d.push(b"data: tail");
    assert_eq!(d.finish().as_deref(), Some("tail"));
}

#[test]
fn api_key_is_redacted_in_debug() {
    let p = OpenAiCompatProvider::with_api_key(
        entry("http://x"),
        OpenAiCompatConfig::default(),
        Some("sk-very-secret".into()),
    )
    .unwrap();
    assert!(!format!("{p:?}").contains("sk-very-secret"));
}

#[test]
fn missing_env_key_is_auth_error() {
    let mut e = entry("http://x");
    e.auth = Auth::ApiKeyEnv("PRAXI_TEST_SURELY_UNSET_KEY".into());
    let err = OpenAiCompatProvider::new(e, OpenAiCompatConfig::default()).unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)));
}

// ---- request mapping ------------------------------------------------------

fn body(e: &ProviderEntry, r: &ChatRequest) -> Value {
    build_body(e, "m", r, true).unwrap()
}

fn tool() -> Value {
    json!({"type":"function","function":{"name":"t","description":"d","parameters":{"type":"object","properties":{}}}})
}

#[test]
fn body_maps_messages_tools_and_limits() {
    let e = entry("http://x");
    let mut r = req();
    r.messages = vec![
        Message::text(MessageRole::System, "sys"),
        Message::text(MessageRole::User, "hi"),
        Message {
            role: MessageRole::Assistant,
            content: Value::Null,
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "t".into(),
                arguments: "{}".into(),
            }],
            tool_call_id: None,
        },
        Message::tool_result("c1", "result"),
    ];
    r.max_tokens = Some(77);
    r.temperature = Some(0.5);
    r.tools = vec![tool()];
    r.tool_choice = Some(ToolChoice::Function("t".into()));
    r.json_mode = true;
    let b = body(&e, &r);
    assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["name"], "t");
    assert_eq!(b["messages"][2]["content"], Value::Null);
    assert_eq!(b["messages"][3]["role"], "tool");
    assert_eq!(b["messages"][3]["tool_call_id"], "c1");
    assert_eq!(b["max_tokens"], 77);
    assert_eq!(b["temperature"], 0.5);
    assert_eq!(b["tool_choice"]["function"]["name"], "t");
    assert_eq!(b["response_format"]["type"], "json_object");
    assert_eq!(b["stream_options"]["include_usage"], true);
    assert!(b.get("parallel_tool_calls").is_none());
}

#[test]
fn compat_flags_change_the_body() {
    let mut e = entry("http://x");
    e.compat_flags = CompatFlags {
        no_parallel_tool_calls: true,
        tools_require_strict: true,
        max_tokens_field: Some("max_completion_tokens".into()),
        no_stream_usage: true,
        system_as_user: true,
        ..CompatFlags::default()
    };
    let mut r = req();
    r.messages
        .insert(0, Message::text(MessageRole::System, "sys"));
    r.max_tokens = Some(9);
    r.tools = vec![tool()];
    let b = body(&e, &r);
    assert_eq!(b["parallel_tool_calls"], false);
    assert_eq!(b["tools"][0]["function"]["strict"], true);
    assert_eq!(
        b["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(b["max_completion_tokens"], 9);
    assert!(b.get("max_tokens").is_none());
    assert!(b.get("stream_options").is_none());
    assert_eq!(b["messages"][0]["role"], "user");
    assert_eq!(b["messages"][0]["content"], "sys");
}

#[test]
fn strip_system_role_merges_into_first_user_message() {
    let mut e = entry("http://x");
    e.compat_flags.strip_system_role = true;
    let mut r = req();
    r.messages
        .insert(0, Message::text(MessageRole::System, "sys"));
    let b = body(&e, &r);
    assert_eq!(b["messages"].as_array().unwrap().len(), 1);
    assert_eq!(b["messages"][0]["role"], "user");
    assert_eq!(b["messages"][0]["content"], "sys\n\nhi");
}

#[test]
fn images_require_vision_and_tools_require_tools() {
    let parts = json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}]);
    let mut r = req();
    r.messages = vec![Message {
        content: parts.clone(),
        ..Message::text(MessageRole::User, "")
    }];
    let mut e = entry("http://x");
    assert!(matches!(
        build_body(&e, "m", &r, true),
        Err(ProviderError::InvalidRequest(_))
    ));
    e.capabilities.vision = true;
    let b = build_body(&e, "m", &r, true).unwrap();
    assert_eq!(b["messages"][0]["content"], parts);

    e.capabilities.tools = false;
    let mut r2 = req();
    r2.tools = vec![tool()];
    assert!(matches!(
        build_body(&e, "m", &r2, true),
        Err(ProviderError::InvalidRequest(_))
    ));
}

#[test]
fn json_mode_ignored_when_unsupported() {
    let mut e = entry("http://x");
    e.capabilities.json_mode = false;
    let mut r = req();
    r.json_mode = true;
    assert!(body(&e, &r).get("response_format").is_none());
}

#[test]
fn compat_flags_deserialize_partially() {
    let f: CompatFlags = toml::from_str("system_as_user = true").unwrap();
    assert!(f.system_as_user && !f.strip_system_role);
}

// ---- live integration (opt-in) -------------------------------------------

/// Runs against a real endpoint. Set `BOT_TEST_PROVIDER` (path to a catalog
/// `.toml`), `BOT_TEST_MODEL`, and the API-key env var that file names.
#[tokio::test]
#[ignore = "needs BOT_TEST_PROVIDER, BOT_TEST_MODEL and an API key"]
async fn live_provider_roundtrip() {
    let (Ok(path), Ok(model)) = (
        std::env::var("BOT_TEST_PROVIDER"),
        std::env::var("BOT_TEST_MODEL"),
    ) else {
        eprintln!("BOT_TEST_PROVIDER / BOT_TEST_MODEL not set; skipping");
        return;
    };
    let entry: ProviderEntry = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let id = entry.id.clone();
    let p = OpenAiCompatProvider::new(entry, OpenAiCompatConfig::default()).unwrap();
    let mut r = ChatRequest::new(
        ModelRef(format!("{id}/{model}")),
        vec![Message::text(
            MessageRole::User,
            "Reply with the single word: pong",
        )],
    );
    r.max_tokens = Some(32);
    let chunks: Vec<_> = p.stream(r).await.unwrap().try_collect().await.unwrap();
    assert!(chunks.iter().any(|c| matches!(c, ChatChunk::TextDelta(_))));
    assert!(matches!(chunks.last(), Some(ChatChunk::Done { .. })));
}
