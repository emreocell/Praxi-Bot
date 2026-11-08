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
    config::ProviderSettings,
    entry::{
        Auth, CapabilityOverrides, CompatFlags, ModelCapabilities, ModelInfo, ProviderEntry,
        ProviderKind,
    },
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
        models: Vec::new(),
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

#[test]
fn audit_extra_exhausted_call_index_returns_error_instead_of_panicking() {
    let mut assembler = super::sse::Assembler::default();
    assembler
        .feed(
            &json!({"choices": [{"delta": {"tool_calls": [{
                "index": u64::MAX, "id": "existing", "function": {"name": "read", "arguments": "{}"}
            }]}}]})
            .to_string(),
        )
        .unwrap();
    let next = json!({"choices": [{"delta": {"tool_calls": [{
        "id": "next", "function": {"name": "read", "arguments": "{}"}
    }]}}]})
    .to_string();
    assert!(matches!(
        assembler.feed(&next),
        Err(ProviderError::Stream(_))
    ));
}

#[test]
fn audit_extra_max_index_continuations_do_not_allocate_a_new_slot() {
    let mut assembler = super::sse::Assembler::default();
    for call in [
        json!({"index": u64::MAX, "id": "existing", "function": {"name": "read", "arguments": "{"}}),
        json!({"id": "existing", "function": {"arguments": "\"x\":1"}}),
        json!({"function": {"arguments": "}"}}),
    ] {
        assembler
            .feed(&json!({"choices": [{"delta": {"tool_calls": [call]}}]}).to_string())
            .unwrap();
    }
    assembler.feed("[DONE]").unwrap();
    let chunks = assembler.finish().unwrap();
    let calls: Vec<_> = chunks
        .iter()
        .filter_map(|chunk| {
            if let ChatChunk::ToolCall(call) = chunk {
                Some(call)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "existing");
    assert_eq!(calls[0].arguments, r#"{"x":1}"#);
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
            cached_tokens: Some(4),
            cache_write_tokens: None,
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
            cached_tokens: None,
            cache_write_tokens: None,
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
                provider_extra: None,
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
        Err(ProviderError::Unsupported { capability }) if capability == "vision"
    ));
    e.capabilities.vision = true;
    let b = build_body(&e, "m", &r, true).unwrap();
    assert_eq!(b["messages"][0]["content"], parts);

    e.capabilities.tools = false;
    let mut r2 = req();
    r2.tools = vec![tool()];
    assert!(matches!(
        build_body(&e, "m", &r2, true),
        Err(ProviderError::Unsupported { capability }) if capability == "tools"
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
fn catalog_model_override_rejects_only_that_model() {
    let src = r#"
id = "agg"
kind = "open_ai_compatible"
base_url = "http://localhost:1/v1"
auth = "None"

[capabilities]
tools = true
vision = true
streaming = true
json_mode = true

[[models]]
id = "weak"
capabilities = { tools = false, vision = false }

[[models]]
id = "strong"
"#;
    let e: ProviderEntry = toml::from_str(src).unwrap();
    let mut r = req();
    r.tools = vec![tool()];
    assert!(matches!(
        build_body(&e, "weak", &r, true),
        Err(ProviderError::Unsupported { ref capability }) if capability == "tools"
    ));
    assert!(build_body(&e, "strong", &r, true).is_ok());
    assert!(build_body(&e, "unlisted", &r, true).is_ok());

    let mut user = ProviderSettings::new("agg", 1);
    user.model_caps.insert(
        "weak".into(),
        CapabilityOverrides {
            tools: Some(true),
            ..CapabilityOverrides::default()
        },
    );
    let merged = user.apply_model_caps(&e);
    assert!(build_body(&merged, "weak", &r, true).is_ok());
    // The catalog vision override survives a user value that does not mention it.
    assert!(!merged.caps_for("weak").vision);
    assert!(merged.caps_for("strong").tools);
}

#[tokio::test]
async fn per_model_streaming_override_disables_sse() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"model": "weak", "stream": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut e = entry(&server.uri());
    e.models.push(ModelInfo {
        id: "weak".into(),
        capabilities: Some(CapabilityOverrides {
            streaming: Some(false),
            ..CapabilityOverrides::default()
        }),
        ..ModelInfo::default()
    });
    assert!(e.caps_for("some-model").streaming);
    assert!(!e.caps_for("weak").streaming);
    let p = OpenAiCompatProvider::with_api_key(e, fast(), None).unwrap();
    let mut r = req();
    r.model = ModelRef("mock/weak".into());
    let chunks = collect(&p, r).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "hi"));
}

#[tokio::test]
async fn tool_support_400s_from_ollama_and_lmstudio_are_unsupported() {
    // Ollama's OpenAI-compatible endpoint (registry form, HTTP 400).
    let ollama = r#"{"error":{"message":"registry.ollama.ai/library/llama3.2:latest does not support tools","type":"api_error","param":null,"code":null}}"#;
    // LM Studio's OpenAI-compatible server (`model_not_support_tools`).
    let lmstudio = r#"{"error":{"message":"The model does not support tools.","type":"invalid_request_error","code":"model_not_support_tools"}}"#;
    let unrelated = r#"{"error":{"message":"invalid temperature","type":"invalid_request_error"}}"#;
    for (body, unsupported) in [(ollama, true), (lmstudio, true), (unrelated, false)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let err = provider(&server).stream(req()).await.err().unwrap();
        if unsupported {
            assert!(
                matches!(err, ProviderError::Unsupported { ref capability } if capability == "tools"),
                "{body} -> {err:?}"
            );
        } else {
            assert!(
                matches!(err, ProviderError::Api { status: 400, .. }),
                "{body} -> {err:?}"
            );
        }
    }
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

#[tokio::test]
async fn thought_signature_is_captured_and_echoed_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","extra_content":{"google":{"thought_signature":"SIG=="}},"function":{"name":"t","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server), req()).await.unwrap();
    let call = chunks
        .iter()
        .find_map(|c| match c {
            ChatChunk::ToolCall(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        call.provider_extra,
        Some(json!({"google":{"thought_signature":"SIG=="}}))
    );
    let mut r = req();
    r.messages.push(Message {
        role: MessageRole::Assistant,
        content: Value::Null,
        tool_calls: vec![call],
        tool_call_id: None,
    });
    r.messages.push(Message::tool_result("c1", "ok"));
    let b = body(&entry("http://x"), &r);
    assert_eq!(
        b["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "SIG=="
    );
}

#[test]
fn invalid_arguments_are_replaced_and_original_kept_in_tool_message() {
    let mut r = req();
    r.messages.push(Message {
        role: MessageRole::Assistant,
        content: Value::Null,
        tool_calls: vec![
            ToolCall::new("c1", "t", "{broken"),
            ToolCall::new("c2", "t", r#"{"a":1}"#),
        ],
        tool_call_id: None,
    });
    r.messages.push(Message::tool_result("c1", "result1"));
    r.messages.push(Message::tool_result("c2", "result2"));
    let b = body(&entry("http://x"), &r);
    assert_eq!(
        b["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(
        b["messages"][1]["tool_calls"][1]["function"]["arguments"],
        r#"{"a":1}"#
    );
    let t1 = b["messages"][2]["content"].as_str().unwrap();
    assert!(t1.contains("{broken") && t1.contains("result1"));
    assert_eq!(b["messages"][3]["content"], "result2");
}

#[tokio::test]
async fn abnormal_finish_reasons_are_typed_errors() {
    for (reason, kind) in [
        ("content_filter", 1),
        ("function_call_filter", 1),
        ("SAFETY", 1),
        ("MALFORMED_FUNCTION_CALL", 2),
        ("weird_new_reason", 3),
    ] {
        let server = MockServer::start().await;
        let ev = format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{reason}"}}]}}"#);
        Mock::given(method("POST"))
            .respond_with(sse(&[&ev, "[DONE]"]))
            .mount(&server)
            .await;
        let e = collect(&provider(&server), req()).await.unwrap_err();
        match kind {
            1 => assert!(matches!(e, ProviderError::ContentFilter(_)), "{reason}"),
            2 => assert!(matches!(e, ProviderError::MalformedToolCall(_)), "{reason}"),
            _ => assert!(matches!(e, ProviderError::Stream(_)), "{reason}"),
        }
    }
}

#[tokio::test]
async fn thought_text_and_flagged_parts_are_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"hidden","extra_content":{"google":{"thought":true}}}}]}"#,
            r#"{"choices":[{"delta":{"content":"A<thought>why"}}]}"#,
            r#"{"choices":[{"delta":{"content":"</thought>B"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]))
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
    assert_eq!(text, "AB");
}

#[tokio::test]
async fn retry_time_is_capped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let cfg = OpenAiCompatConfig {
        max_retries: 1000,
        base_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(20),
        max_total_retry: Duration::from_millis(100),
        ..OpenAiCompatConfig::default()
    };
    let p = OpenAiCompatProvider::with_api_key(entry(&server.uri()), cfg, None).unwrap();
    let t = std::time::Instant::now();
    assert!(collect(&p, req()).await.is_err());
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(server.received_requests().await.unwrap().len() < 30);
}

#[tokio::test]
async fn parallel_tool_calls_without_index_stay_separate() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a","type":"function","function":{"name":"read_file","arguments":"{\"a\":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_b","type":"function","function":{"name":"list_dir","arguments":"{\"b\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"2}"}}]}}]}"#,
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
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        ("call_a", "read_file")
    );
    assert_eq!(calls[0].arguments, r#"{"a":1}"#);
    assert_eq!(
        (calls[1].id.as_str(), calls[1].name.as_str()),
        ("call_b", "list_dir")
    );
    assert_eq!(calls[1].arguments, r#"{"b":2}"#);
}

#[tokio::test]
async fn retry_after_longer_than_max_delay_fails_immediately() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .expect(1) // no futile retries
        .mount(&server)
        .await;
    let started = std::time::Instant::now();
    let err = provider(&server).stream(req()).await.err().unwrap();
    assert!(matches!(
        err,
        ProviderError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(3600)
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
}
