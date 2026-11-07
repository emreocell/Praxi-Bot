use std::{fmt::Write as _, time::Duration};

use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

use super::{AnthropicProvider, request::build_body, stream::Decoder};
use crate::{
    entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind},
    error::ProviderError,
    openai_compat::OpenAiCompatConfig,
    provider::{
        ChatChunk, ChatProvider, ChatRequest, FinishReason, Message, MessageRole, ModelRef,
        ToolCall, ToolChoice, Usage,
    },
    util::EventDecoder,
};

fn entry(base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: "anthropic".into(),
        kind: ProviderKind::Anthropic,
        base_url: base_url.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: true,
            streaming: true,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    }
}

fn fast(retries: u32) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        max_retries: retries,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        ..OpenAiCompatConfig::default()
    }
}

fn provider(server: &MockServer, retries: u32) -> AnthropicProvider {
    AnthropicProvider::with_api_key(
        entry(&server.uri()),
        fast(retries),
        Some("sk-ant-secret".into()),
    )
    .unwrap()
}

fn req() -> ChatRequest {
    ChatRequest::new(
        ModelRef("anthropic/claude-test".into()),
        vec![Message::text(MessageRole::User, "hi")],
    )
}

/// SSE body with `event:` lines like the real API.
fn sse(events: &[Value]) -> ResponseTemplate {
    let mut body = String::new();
    for e in events {
        write!(
            body,
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap()
        )
        .unwrap();
    }
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn start(usage: &Value) -> Value {
    json!({"type":"message_start","message":{"id":"msg_1","role":"assistant","usage":usage}})
}

fn text_start(i: u64) -> Value {
    json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}})
}

fn text_delta(i: u64, t: &str) -> Value {
    json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":t}})
}

fn stop(i: u64) -> Value {
    json!({"type":"content_block_stop","index":i})
}

fn tool_start(i: u64, id: &str, name: &str) -> Value {
    json!({"type":"content_block_start","index":i,
        "content_block":{"type":"tool_use","id":id,"name":name,"input":{}}})
}

fn json_delta(i: u64, part: &str) -> Value {
    json!({"type":"content_block_delta","index":i,
        "delta":{"type":"input_json_delta","partial_json":part}})
}

fn msg_delta(reason: &str, out: u64) -> Value {
    json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":out}})
}

fn msg_stop() -> Value {
    json!({"type":"message_stop"})
}

async fn collect(p: &AnthropicProvider, r: ChatRequest) -> Result<Vec<ChatChunk>, ProviderError> {
    p.stream(r).await?.try_collect().await
}

fn err_body(kind: &str, msg: &str) -> Value {
    json!({"type":"error","error":{"type":kind,"message":msg}})
}

#[tokio::test]
async fn text_stream_with_headers_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-secret"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(sse(&[
            start(&json!({"input_tokens":12,"output_tokens":1})),
            json!({"type":"ping"}),
            text_start(0),
            text_delta(0, "Hel"),
            text_delta(0, "lo"),
            stop(0),
            msg_delta("end_turn", 7),
            msg_stop(),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "Hel"));
    assert!(matches!(&chunks[1], ChatChunk::TextDelta(t) if t == "lo"));
    assert!(matches!(
        &chunks[2],
        ChatChunk::Usage(Usage {
            prompt_tokens: 12,
            completion_tokens: 7,
            cached_tokens: None,
            cache_write_tokens: None
        })
    ));
    assert!(matches!(
        &chunks[3],
        ChatChunk::Done {
            finish_reason: FinishReason::Stop
        }
    ));
    assert_eq!(chunks.len(), 4);
}

#[tokio::test]
async fn request_body_is_streaming_messages_with_defaults() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(wiremock::matchers::body_partial_json(json!({
            "model": "claude-test", "stream": true, "max_tokens": 8192,
            "messages": [{"role":"user","content":[{"type":"text","text":"hi"}]}]
        })))
        .respond_with(sse(&[
            start(&json!({})),
            msg_delta("end_turn", 0),
            msg_stop(),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    collect(&provider(&server, 0), req()).await.unwrap();
}

#[tokio::test]
async fn tool_call_with_streamed_args() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            start(&json!({"input_tokens":5})),
            text_start(0),
            text_delta(0, "Checking."),
            stop(0),
            tool_start(1, "toolu_1", "get_weather"),
            json_delta(1, ""),
            json_delta(1, "{\"city\":"),
            json_delta(1, "\"Paris\"}"),
            stop(1),
            msg_delta("tool_use", 20),
            msg_stop(),
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "Checking."));
    // Start delta names the call; later deltas carry the JSON pieces.
    assert!(matches!(
        &chunks[1],
        ChatChunk::ToolCallDelta { call_id, name: Some(n), input_delta }
            if call_id == "toolu_1" && n == "get_weather" && input_delta.is_empty()
    ));
    let pieces: Vec<&str> = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::ToolCallDelta {
                name: None,
                input_delta,
                ..
            } => Some(input_delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(pieces, ["", "{\"city\":", "\"Paris\"}"]);
    let call = chunks
        .iter()
        .find_map(|c| match c {
            ChatChunk::ToolCall(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        call,
        ToolCall {
            id: "toolu_1".into(),
            name: "get_weather".into(),
            arguments: "{\"city\":\"Paris\"}".into(),
            provider_extra: None,
        }
    );
    assert!(matches!(
        chunks.last().unwrap(),
        ChatChunk::Done {
            finish_reason: FinishReason::ToolCalls
        }
    ));
}

#[tokio::test]
async fn parallel_tool_calls_interleaved_and_no_arg_tool() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            start(&json!({"input_tokens":5})),
            tool_start(0, "toolu_a", "read"),
            tool_start(1, "toolu_b", "list"),
            json_delta(0, "{\"p\":"),
            json_delta(0, "1}"),
            stop(0),
            stop(1),
            msg_delta("tool_use", 9),
            msg_stop(),
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let calls: Vec<ToolCall> = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::ToolCall(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].arguments.as_str()),
        ("toolu_a", "{\"p\":1}")
    );
    // A tool without input streams no JSON: arguments default to `{}`.
    assert_eq!(
        (calls[1].id.as_str(), calls[1].arguments.as_str()),
        ("toolu_b", "{}")
    );
}

#[tokio::test]
async fn cache_usage_accounting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            start(&json!({
                "input_tokens": 10,
                "cache_creation_input_tokens": 300,
                "cache_read_input_tokens": 2000,
                "output_tokens": 1
            })),
            text_start(0),
            text_delta(0, "ok"),
            stop(0),
            msg_delta("end_turn", 42),
            msg_stop(),
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let usage = chunks
        .iter()
        .find_map(|c| match c {
            ChatChunk::Usage(u) => Some(*u),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        usage,
        Usage {
            prompt_tokens: 2310,
            completion_tokens: 42,
            cached_tokens: Some(2000),
            cache_write_tokens: Some(300)
        }
    );
}

#[test]
fn thinking_blocks_are_kept_but_not_emitted() {
    let mut d = Decoder::default();
    let mut out = Vec::new();
    for e in [
        start(&json!({"input_tokens":1})),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm "}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ok"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}}),
        stop(0),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        text_start(2),
        text_delta(2, "answer"),
        msg_delta("end_turn", 3),
        msg_stop(),
    ] {
        out.extend(d.feed(&e.to_string()).unwrap());
    }
    assert!(matches!(out.as_slice(), [ChatChunk::TextDelta(t)] if t == "answer"));
    let th = d.thinking();
    assert_eq!(th.len(), 2);
    assert_eq!(
        (th[0].text.as_str(), th[0].signature.as_str()),
        ("hmm ok", "sig==")
    );
    assert_eq!(th[1].redacted.as_deref(), Some("opaque"));
}

#[tokio::test]
async fn finish_reasons_map() {
    for (reason, want) in [
        ("end_turn", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
    ] {
        let mut d = Decoder::default();
        d.feed(&msg_delta(reason, 1).to_string()).unwrap();
        d.feed(&msg_stop().to_string()).unwrap();
        let out = d.finish().unwrap();
        assert!(
            matches!(out.last(), Some(ChatChunk::Done { finish_reason }) if *finish_reason == want),
            "{reason}"
        );
    }
}

#[tokio::test]
async fn refusal_and_unknown_stop_reasons_are_errors() {
    let run = |reason: &str| {
        let mut d = Decoder::default();
        d.feed(&msg_delta(reason, 1).to_string()).unwrap();
        d.feed(&msg_stop().to_string()).unwrap();
        d.finish()
    };
    assert!(matches!(
        run("refusal"),
        Err(ProviderError::ContentFilter(_))
    ));
    assert!(matches!(run("pause_turn"), Err(ProviderError::Stream(_))));
    assert!(matches!(
        run("brand_new_reason"),
        Err(ProviderError::Stream(_))
    ));
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            start(&json!({})),
            text_start(0),
            text_delta(0, "par"),
        ]))
        .mount(&server)
        .await;
    let err = collect(&provider(&server, 0), req()).await.unwrap_err();
    assert!(matches!(err, ProviderError::Stream(_)));
}

async fn status_error(
    status: u16,
    body: Value,
    retries: u32,
    extra: Option<(&str, &str)>,
) -> ProviderError {
    let server = MockServer::start().await;
    let mut resp = ResponseTemplate::new(status).set_body_json(body);
    if let Some((k, v)) = extra {
        resp = resp.insert_header(k, v);
    }
    Mock::given(method("POST"))
        .respond_with(resp)
        .mount(&server)
        .await;
    collect(&provider(&server, retries), req())
        .await
        .unwrap_err()
}

#[tokio::test]
async fn http_error_kinds() {
    let e = status_error(
        401,
        err_body("authentication_error", "invalid x-api-key"),
        2,
        None,
    )
    .await;
    assert!(matches!(e, ProviderError::Auth(m) if m == "invalid x-api-key"));

    let e = status_error(403, err_body("permission_error", "no"), 2, None).await;
    assert!(matches!(e, ProviderError::Auth(_)));

    let e = status_error(
        400,
        err_body("invalid_request_error", "max_tokens: too big"),
        2,
        None,
    )
    .await;
    assert!(matches!(e, ProviderError::InvalidRequest(m) if m.contains("max_tokens")));

    let e = status_error(
        429,
        err_body("rate_limit_error", "slow"),
        0,
        Some(("retry-after", "7")),
    )
    .await;
    assert!(matches!(
        e,
        ProviderError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(7)
    ));

    let e = status_error(529, err_body("overloaded_error", "Overloaded"), 0, None).await;
    assert!(matches!(e, ProviderError::Upstream { status: 529, .. }));

    let e = status_error(500, err_body("api_error", "boom"), 0, None).await;
    assert!(matches!(e, ProviderError::Upstream { status: 500, .. }));

    let e = status_error(404, err_body("not_found_error", "model"), 2, None).await;
    assert!(matches!(e, ProviderError::Api { status: 404, .. }));
}

#[tokio::test]
async fn overloaded_and_rate_limit_are_retried() {
    for (status, kind) in [(529, "overloaded_error"), (429, "rate_limit_error")] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(err_body(kind, "x")))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(sse(&[
                start(&json!({})),
                msg_delta("end_turn", 0),
                msg_stop(),
            ]))
            .expect(1)
            .mount(&server)
            .await;
        let chunks = collect(&provider(&server, 3), req()).await.unwrap();
        assert!(matches!(chunks.last(), Some(ChatChunk::Done { .. })));
    }
}

#[tokio::test]
async fn invalid_request_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(err_body("invalid_request_error", "bad")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let e = collect(&provider(&server, 3), req()).await.unwrap_err();
    assert!(matches!(e, ProviderError::InvalidRequest(_)));
}

#[tokio::test]
async fn in_stream_error_events_are_mapped() {
    for (kind, check) in [
        ("overloaded_error", 0),
        ("rate_limit_error", 1),
        ("authentication_error", 2),
        ("invalid_request_error", 3),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(&[
                start(&json!({})),
                text_start(0),
                text_delta(0, "a"),
                err_body(kind, "m"),
            ]))
            .mount(&server)
            .await;
        let err = collect(&provider(&server, 0), req()).await.unwrap_err();
        let ok = match check {
            0 => matches!(err, ProviderError::Upstream { status: 529, .. }),
            1 => matches!(err, ProviderError::RateLimited { .. }),
            2 => matches!(err, ProviderError::Auth(_)),
            _ => matches!(err, ProviderError::InvalidRequest(_)),
        };
        assert!(ok, "{kind}: {err:?}");
    }
}

#[tokio::test]
async fn list_models_reads_data_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-ant-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id":"claude-a","type":"model"},{"id":"claude-b"}], "has_more": false
        })))
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server, 0).list_models().await.unwrap(),
        ["claude-a", "claude-b"]
    );
}

#[test]
fn base_url_with_v1_suffix_is_accepted() {
    let p = AnthropicProvider::with_api_key(entry("https://x.example/v1/"), fast(0), None).unwrap();
    assert_eq!(p.base(), "https://x.example");
}

#[test]
fn debug_output_redacts_key() {
    let p =
        AnthropicProvider::with_api_key(entry("http://x"), fast(0), Some("sk-ant-secret".into()))
            .unwrap();
    assert!(!format!("{p:?}").contains("sk-ant-secret"));
}

// ---- request building ----

fn body(r: &ChatRequest, caching: bool) -> Value {
    build_body(&entry("http://x"), "m", r, caching).unwrap()
}

fn tool_def(name: &str) -> Value {
    json!({"type":"function","function":{"name":name,"description":"d",
        "parameters":{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}}})
}

#[test]
fn system_tools_and_last_block_get_cache_control() {
    let mut r = req();
    r.messages = vec![
        Message::text(MessageRole::System, "be brief"),
        Message::text(MessageRole::System, "be kind"),
        Message::text(MessageRole::User, "q1"),
        Message::text(MessageRole::Assistant, "a1"),
        Message::text(MessageRole::User, "q2"),
    ];
    r.tools = vec![tool_def("one"), tool_def("two")];
    let b = body(&r, true);
    let cc = json!({"type":"ephemeral"});
    assert_eq!(b["system"][0].get("cache_control"), None);
    assert_eq!(b["system"][1]["cache_control"], cc);
    assert_eq!(b["system"][1]["text"], "be kind");
    assert_eq!(b["tools"][0].get("cache_control"), None);
    assert_eq!(b["tools"][1]["cache_control"], cc);
    assert_eq!(b["tools"][1]["input_schema"]["required"][0], "a");
    assert_eq!(b["tools"][1]["description"], "d");
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["content"][0].get("cache_control"), None);
    assert_eq!(msgs[2]["content"][0]["cache_control"], cc);
    // Never more than 4 breakpoints.
    assert_eq!(b.to_string().matches("cache_control").count(), 3);
}

#[test]
fn caching_can_be_disabled() {
    let mut r = req();
    r.messages
        .insert(0, Message::text(MessageRole::System, "s"));
    r.tools = vec![tool_def("t")];
    assert!(!body(&r, false).to_string().contains("cache_control"));
}

#[test]
fn tool_round_trip_messages_merge_results_into_one_user_message() {
    let mut asst = Message::text(MessageRole::Assistant, "");
    asst.content = Value::Null;
    asst.tool_calls = vec![
        ToolCall {
            id: "t1".into(),
            name: "a".into(),
            arguments: "{\"x\":1}".into(),
            provider_extra: None,
        },
        ToolCall {
            id: "t2".into(),
            name: "b".into(),
            arguments: "not json".into(),
            provider_extra: None,
        },
    ];
    let mut r = req();
    r.messages = vec![
        Message::text(MessageRole::User, "go"),
        asst,
        Message::tool_result("t1", "r1"),
        Message::tool_result("t2", "r2"),
        Message::text(MessageRole::User, "thanks"),
    ];
    let b = body(&r, false);
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!(m[1]["content"][0]["type"], "tool_use");
    assert_eq!(m[1]["content"][0]["input"], json!({"x":1}));
    // Invalid argument JSON degrades to an empty object instead of a 400.
    assert_eq!(m[1]["content"][1]["input"], json!({}));
    let last = m[2]["content"].as_array().unwrap();
    assert_eq!(last.len(), 3);
    assert_eq!(last[0]["tool_use_id"], "t1");
    assert_eq!(last[1]["tool_use_id"], "t2");
    assert_eq!(last[2]["type"], "text");
}

#[test]
fn images_become_base64_blocks() {
    let mut r = req();
    r.messages = vec![Message {
        role: MessageRole::User,
        content: json!([
            {"type":"text","text":"what is this"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},
            {"type":"image_url","image_url":{"url":"https://x.example/a.jpg"}}
        ]),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];
    let b = body(&r, false);
    let c = &b["messages"][0]["content"];
    assert_eq!(
        c[1],
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}})
    );
    assert_eq!(
        c[2]["source"],
        json!({"type":"url","url":"https://x.example/a.jpg"})
    );

    let mut e = entry("http://x");
    e.capabilities.vision = false;
    assert!(matches!(
        build_body(&e, "m", &r, false),
        Err(ProviderError::Unsupported { capability }) if capability == "vision"
    ));
}

#[test]
fn per_model_tool_override_is_unsupported_for_that_model_only() {
    let mut e = entry("http://x");
    e.models.push(crate::entry::ModelInfo {
        id: "weak".into(),
        capabilities: Some(crate::entry::CapabilityOverrides {
            tools: Some(false),
            ..crate::entry::CapabilityOverrides::default()
        }),
        ..crate::entry::ModelInfo::default()
    });
    let mut r = req();
    r.tools = vec![tool_def("t")];
    assert!(matches!(
        build_body(&e, "weak", &r, false),
        Err(ProviderError::Unsupported { capability }) if capability == "tools"
    ));
    assert!(build_body(&e, "strong", &r, false).is_ok());
}

#[test]
fn tool_choice_mapping_and_no_parallel_flag() {
    let mut r = req();
    r.tools = vec![tool_def("t")];
    for (c, want) in [
        (ToolChoice::Auto, json!({"type":"auto"})),
        (ToolChoice::None, json!({"type":"none"})),
        (ToolChoice::Required, json!({"type":"any"})),
        (
            ToolChoice::Function("t".into()),
            json!({"type":"tool","name":"t"}),
        ),
    ] {
        r.tool_choice = Some(c);
        assert_eq!(body(&r, false)["tool_choice"], want);
    }
    r.tool_choice = None;
    assert!(body(&r, false).get("tool_choice").is_none());
    let mut e = entry("http://x");
    e.compat_flags.no_parallel_tool_calls = true;
    let b = build_body(&e, "m", &r, false).unwrap();
    assert_eq!(
        b["tool_choice"],
        json!({"type":"auto","disable_parallel_tool_use":true})
    );
}

#[test]
fn explicit_max_tokens_and_temperature_pass_through() {
    let mut r = req();
    r.max_tokens = Some(1);
    r.temperature = Some(0.2);
    let b = body(&r, false);
    assert_eq!(b["max_tokens"], 1);
    assert_eq!(b["temperature"], 0.2);
}

/// Live test: runs only with `ANTHROPIC_API_KEY` set (and `ANTHROPIC_TEST_MODEL`
/// to override the model).
#[tokio::test]
#[ignore = "needs ANTHROPIC_API_KEY and network"]
async fn live_anthropic_smoke() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        return;
    };
    let model = std::env::var("ANTHROPIC_TEST_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into());
    let p = AnthropicProvider::with_api_key(
        entry("https://api.anthropic.com"),
        OpenAiCompatConfig::default(),
        Some(key),
    )
    .unwrap();
    let mut r = ChatRequest::new(
        ModelRef(format!("anthropic/{model}")),
        vec![Message::text(
            MessageRole::User,
            "Reply with the single word: pong",
        )],
    );
    r.max_tokens = Some(64);
    let chunks = collect(&p, r).await.unwrap();
    assert!(chunks.iter().any(|c| matches!(c, ChatChunk::Done { .. })));
}
