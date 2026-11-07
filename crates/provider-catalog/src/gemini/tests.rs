use std::{fmt::Write as _, time::Duration};

use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

use super::{
    GeminiProvider,
    request::{build_body, make_call_id, signature_of},
    schema::convert_parameters,
};
use crate::{
    entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind},
    error::ProviderError,
    openai_compat::OpenAiCompatConfig,
    provider::{
        ChatChunk, ChatProvider, ChatRequest, FinishReason, Message, MessageRole, ModelRef,
        ToolCall, ToolChoice, Usage,
    },
};

fn entry(base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: "gemini".into(),
        kind: ProviderKind::Gemini,
        base_url: base_url.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: true,
            streaming: true,
            json_mode: true,
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

fn provider(server: &MockServer, retries: u32) -> GeminiProvider {
    GeminiProvider::with_api_key(entry(&server.uri()), fast(retries), Some("g-secret".into()))
        .unwrap()
}

fn req() -> ChatRequest {
    ChatRequest::new(
        ModelRef("gemini/gemini-test".into()),
        vec![Message::text(MessageRole::User, "hi")],
    )
}

fn sse(events: &[Value]) -> ResponseTemplate {
    let mut body = String::new();
    for e in events {
        write!(body, "data: {e}\r\n\r\n").unwrap();
    }
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn text_chunk(t: &str) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":[{"text":t}]},"index":0}]})
}

fn final_chunk(parts: &[Value], reason: &str, usage: &Value) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":reason,"index":0}],
        "usageMetadata": usage})
}

fn call(name: &str, args: &Value) -> Value {
    json!({"functionCall":{"name":name,"args":args}})
}

async fn collect(p: &GeminiProvider, r: ChatRequest) -> Result<Vec<ChatChunk>, ProviderError> {
    p.stream(r).await?.try_collect().await
}

fn tool_calls(chunks: &[ChatChunk]) -> Vec<ToolCall> {
    chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::ToolCall(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn text_stream_uses_sse_endpoint_and_key_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/gemini-test:streamGenerateContent"))
        .and(query_param("alt", "sse"))
        .and(header("x-goog-api-key", "g-secret"))
        .respond_with(sse(&[
            text_chunk("Hel"),
            text_chunk("lo"),
            final_chunk(
                &[json!({"text":"!"})],
                "STOP",
                &json!({"promptTokenCount":9,"candidatesTokenCount":3,"totalTokenCount":12}),
            ),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let text: String = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello!");
    assert!(matches!(
        chunks[3],
        ChatChunk::Usage(Usage {
            prompt_tokens: 9,
            completion_tokens: 3,
            cached_tokens: None,
            cache_write_tokens: None
        })
    ));
    assert!(matches!(
        chunks[4],
        ChatChunk::Done {
            finish_reason: FinishReason::Stop
        }
    ));
}

#[tokio::test]
async fn thought_parts_are_not_answer_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(
            &[
                json!({"text":"thinking...","thought":true}),
                json!({"text":"answer"}),
            ],
            "STOP",
            &json!({}),
        )]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "answer"));
    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, ChatChunk::TextDelta(t) if t.contains("thinking")))
    );
}

#[tokio::test]
async fn function_call_becomes_tool_call_and_tool_calls_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            text_chunk("Looking."),
            final_chunk(
                &[call("get_weather", &json!({"city":"Paris"}))],
                "STOP",
                &json!({"promptTokenCount":4,"candidatesTokenCount":2}),
            ),
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let calls = tool_calls(&chunks);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].arguments).unwrap(),
        json!({"city":"Paris"})
    );
    assert!(calls[0].id.starts_with("gm"));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ChatChunk::ToolCallDelta { call_id, name: Some(n), .. }
            if *call_id == calls[0].id && n == "get_weather"
    )));
    // STOP with a function call means "run the tools".
    assert!(matches!(
        chunks.last().unwrap(),
        ChatChunk::Done {
            finish_reason: FinishReason::ToolCalls
        }
    ));
}

#[tokio::test]
async fn parallel_calls_have_distinct_ids_and_keep_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            json!({"candidates":[{"content":{"parts":[call("a", &json!({"x":1}))]}}]}),
            final_chunk(
                &[call("b", &json!({})), call("a", &json!({"x":2}))],
                "STOP",
                &json!({}),
            ),
        ]))
        .mount(&server)
        .await;
    let calls = tool_calls(&collect(&provider(&server, 0), req()).await.unwrap());
    let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "a"]);
    let mut ids: Vec<&str> = calls.iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    assert_eq!(calls[1].arguments, "{}");
}

#[tokio::test]
async fn cached_content_and_thinking_token_accounting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(
            &[json!({"text":"ok"})],
            "STOP",
            &json!({"promptTokenCount":1000,"cachedContentTokenCount":800,
                    "candidatesTokenCount":20,"thoughtsTokenCount":30,"totalTokenCount":1050}),
        )]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let u = chunks
        .iter()
        .find_map(|c| match c {
            ChatChunk::Usage(u) => Some(*u),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        u,
        Usage {
            prompt_tokens: 1000,
            completion_tokens: 50,
            cached_tokens: Some(800),
            cache_write_tokens: None
        }
    );
}

#[test]
fn signature_survives_id_round_trip() {
    let id = make_call_id(0xdead_beef, Some("ab+/cd=="));
    assert_eq!(id.len(), 2 + 16 + 1 + 6);
    assert!(
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    assert_eq!(signature_of(&id).as_deref(), Some("ab+/cd=="));
    assert_eq!(signature_of(&make_call_id(1, None)), None);
    assert_eq!(signature_of("call_123"), None);
}

#[test]
fn finish_reasons_and_blocked_prompt() {
    use crate::util::EventDecoder;
    let run = |r: &str| {
        let mut d = super::stream::Decoder::default();
        d.feed(&final_chunk(&[], r, &json!({})).to_string())
            .unwrap();
        d.finish()
    };
    let done = run("MAX_TOKENS").unwrap();
    assert!(matches!(
        done.last(),
        Some(ChatChunk::Done {
            finish_reason: FinishReason::Length
        })
    ));
    for r in ["SAFETY", "RECITATION", "BLOCKLIST", "PROHIBITED_CONTENT"] {
        assert!(
            matches!(run(r), Err(ProviderError::ContentFilter(_))),
            "{r}"
        );
    }
    assert!(matches!(
        run("MALFORMED_FUNCTION_CALL"),
        Err(ProviderError::MalformedToolCall(_))
    ));
    assert!(matches!(run("OTHER"), Err(ProviderError::Stream(_))));
    let mut d = super::stream::Decoder::default();
    d.feed(&json!({"promptFeedback":{"blockReason":"SAFETY"}}).to_string())
        .unwrap();
    assert!(matches!(d.finish(), Err(ProviderError::ContentFilter(_))));
}

#[tokio::test]
async fn malformed_function_call_is_retried_once_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(
            &[],
            "MALFORMED_FUNCTION_CALL",
            &json!({}),
        )]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(
            &[json!({"text": "ok"})],
            "STOP",
            &json!({}),
        )]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    assert!(matches!(&chunks[0], ChatChunk::TextDelta(t) if t == "ok"));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_function_call_twice_is_a_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(
            &[],
            "MALFORMED_FUNCTION_CALL",
            &json!({}),
        )]))
        .mount(&server)
        .await;
    let e = collect(&provider(&server, 0), req()).await.unwrap_err();
    assert!(matches!(e, ProviderError::MalformedToolCall(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn safety_finish_is_content_filter_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[final_chunk(&[], "SAFETY", &json!({}))]))
        .mount(&server)
        .await;
    let e = collect(&provider(&server, 0), req()).await.unwrap_err();
    assert!(matches!(e, ProviderError::ContentFilter(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn thought_tags_never_reach_the_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            text_chunk("Hi <thou"),
            text_chunk("ght>secret plan</thou"),
            final_chunk(&[json!({"text": "ght> there"})], "STOP", &json!({})),
        ]))
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server, 0), req()).await.unwrap();
    let text: String = chunks
        .iter()
        .filter_map(|c| match c {
            ChatChunk::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hi  there");
}

#[tokio::test]
async fn retry_time_is_capped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":{"message":"x"}})))
        .mount(&server)
        .await;
    let cfg = OpenAiCompatConfig {
        max_retries: 1000,
        base_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(20),
        max_total_retry: Duration::from_millis(100),
        ..OpenAiCompatConfig::default()
    };
    let p = GeminiProvider::with_api_key(entry(&server.uri()), cfg, None).unwrap();
    let t = std::time::Instant::now();
    let e = collect(&p, req()).await.unwrap_err();
    assert!(matches!(e, ProviderError::Upstream { .. }));
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(server.received_requests().await.unwrap().len() < 30);
}

#[tokio::test]
async fn failed_400_is_dumped_redacted_when_debug_dir_is_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":{"message":"bad"}})))
        .mount(&server)
        .await;
    let dir = std::env::temp_dir().join(format!("gf-dump-{}", std::process::id()));
    let cfg = OpenAiCompatConfig {
        debug_dir: Some(dir.clone()),
        ..fast(0)
    };
    let p =
        GeminiProvider::with_api_key(entry(&server.uri()), cfg, Some("g-secret".into())).unwrap();
    let mut r = req();
    r.messages
        .push(Message::text(MessageRole::User, "x".repeat(5000)));
    assert!(collect(&p, r).await.is_err());
    let text = std::fs::read_to_string(dir.join("last-failed-request-gemini.json")).unwrap();
    assert!(text.contains("bytes redacted"));
    assert!(!text.contains("g-secret"));
    assert!(text.contains("\"status\": 400"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_call_arguments_are_replaced_and_noted_in_the_tool_message() {
    let mut r = req();
    let id = make_call_id(1, None);
    r.messages.push(Message {
        role: MessageRole::Assistant,
        content: Value::Null,
        tool_calls: vec![ToolCall::new(id.clone(), "a", "{not json")],
        tool_call_id: None,
    });
    r.messages.push(Message::tool_result(id, "done"));
    let b = body(&r);
    assert_eq!(
        b["contents"][1]["parts"][0]["functionCall"]["args"],
        json!({})
    );
    let out = b["contents"][2]["parts"][0]["functionResponse"]["response"]["output"]
        .as_str()
        .unwrap();
    assert!(out.contains("{not json") && out.contains("done"));
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[text_chunk("par")]))
        .mount(&server)
        .await;
    assert!(matches!(
        collect(&provider(&server, 0), req()).await.unwrap_err(),
        ProviderError::Stream(_)
    ));
}

#[allow(clippy::needless_pass_by_value)]
fn gerr(code: u16, status: &str, msg: &str, details: Value) -> Value {
    json!({"error":{"code":code,"message":msg,"status":status,"details":details}})
}

async fn status_error(
    status: u16,
    body: Value,
    retries: u32,
    header_kv: Option<(&str, &str)>,
) -> ProviderError {
    let server = MockServer::start().await;
    let mut resp = ResponseTemplate::new(status).set_body_json(body);
    if let Some((k, v)) = header_kv {
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
    let bad_key = gerr(
        400,
        "INVALID_ARGUMENT",
        "API key not valid. Please pass a valid API key.",
        json!([{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"}]),
    );
    assert!(matches!(
        status_error(400, bad_key, 2, None).await,
        ProviderError::Auth(_)
    ));

    let e = status_error(
        403,
        gerr(403, "PERMISSION_DENIED", "denied", json!([])),
        2,
        None,
    )
    .await;
    assert!(matches!(e, ProviderError::Auth(_)));
    let e = status_error(401, gerr(401, "UNAUTHENTICATED", "no", json!([])), 2, None).await;
    assert!(matches!(e, ProviderError::Auth(_)));

    let e = status_error(
        400,
        gerr(400, "INVALID_ARGUMENT", "bad schema", json!([])),
        2,
        None,
    )
    .await;
    assert!(matches!(e, ProviderError::InvalidRequest(m) if m == "bad schema"));

    let e = status_error(
        429,
        gerr(429, "RESOURCE_EXHAUSTED", "quota", json!([])),
        0,
        Some(("retry-after", "9")),
    )
    .await;
    assert!(matches!(
        e,
        ProviderError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(9)
    ));

    // No header: fall back to the RetryInfo detail.
    let info = json!([{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"2.5s"}]);
    let e = status_error(429, gerr(429, "RESOURCE_EXHAUSTED", "quota", info), 0, None).await;
    assert!(matches!(
        e,
        ProviderError::RateLimited { retry_after: Some(d) } if d == Duration::from_millis(2500)
    ));

    let e = status_error(
        503,
        gerr(503, "UNAVAILABLE", "overloaded", json!([])),
        0,
        None,
    )
    .await;
    assert!(matches!(e, ProviderError::Upstream { status: 503, .. }));
    let e = status_error(500, gerr(500, "INTERNAL", "oops", json!([])), 0, None).await;
    assert!(matches!(e, ProviderError::Upstream { status: 500, .. }));
    let e = status_error(404, gerr(404, "NOT_FOUND", "model", json!([])), 2, None).await;
    assert!(matches!(e, ProviderError::Api { status: 404, .. }));
}

#[tokio::test]
async fn unavailable_and_rate_limited_are_retried_but_invalid_is_not() {
    for status in [503u16, 429] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(gerr(
                status,
                "X",
                "x",
                json!([]),
            )))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(sse(&[final_chunk(
                &[json!({"text":"ok"})],
                "STOP",
                &json!({}),
            )]))
            .expect(1)
            .mount(&server)
            .await;
        assert!(collect(&provider(&server, 3), req()).await.is_ok());
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(gerr(
            400,
            "INVALID_ARGUMENT",
            "bad",
            json!([]),
        )))
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        collect(&provider(&server, 3), req()).await.unwrap_err(),
        ProviderError::InvalidRequest(_)
    ));
}

#[tokio::test]
async fn in_stream_error_object_is_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            text_chunk("a"),
            gerr(503, "UNAVAILABLE", "The model is overloaded", json!([])),
        ]))
        .mount(&server)
        .await;
    assert!(matches!(
        collect(&provider(&server, 0), req()).await.unwrap_err(),
        ProviderError::Upstream { status: 503, .. }
    ));
}

#[tokio::test]
async fn list_models_filters_and_strips_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("x-goog-api-key", "g-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[
            {"name":"models/gemini-a","supportedGenerationMethods":["generateContent","countTokens"]},
            {"name":"models/embed-b","supportedGenerationMethods":["embedContent"]},
            {"name":"models/gemini-c"}
        ]})))
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server, 0).list_models().await.unwrap(),
        ["gemini-a", "gemini-c"]
    );
}

#[test]
fn debug_output_redacts_key() {
    let p =
        GeminiProvider::with_api_key(entry("http://x"), fast(0), Some("g-secret".into())).unwrap();
    assert!(!format!("{p:?}").contains("g-secret"));
}

// ---- request building ----

fn body(r: &ChatRequest) -> Value {
    build_body(&entry("http://x"), r).unwrap()
}

fn tool_def(name: &str, params: &Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":"d","parameters":params}})
}

#[test]
fn system_instruction_generation_config_and_json_mode() {
    let mut r = req();
    r.messages
        .insert(0, Message::text(MessageRole::System, "be brief"));
    r.max_tokens = Some(77);
    r.temperature = Some(0.5);
    r.json_mode = true;
    let b = body(&r);
    assert_eq!(
        b["systemInstruction"],
        json!({"parts":[{"text":"be brief"}]})
    );
    assert_eq!(
        b["contents"],
        json!([{"role":"user","parts":[{"text":"hi"}]}])
    );
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 77);
    assert_eq!(b["generationConfig"]["temperature"], 0.5);
    assert_eq!(
        b["generationConfig"]["responseMimeType"],
        "application/json"
    );
}

#[test]
fn tools_and_tool_choice() {
    let mut r = req();
    r.tools = vec![
        tool_def(
            "a",
            &json!({"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}),
        ),
        tool_def("noargs", &json!({"type":"object","properties":{}})),
    ];
    let b = body(&r);
    let decls = &b["tools"][0]["functionDeclarations"];
    assert_eq!(decls[0]["name"], "a");
    assert_eq!(decls[0]["parameters"]["type"], "OBJECT");
    assert_eq!(decls[0]["parameters"]["properties"]["q"]["type"], "STRING");
    // Gemini rejects an empty OBJECT schema: parameters must be omitted.
    assert!(decls[1].get("parameters").is_none());
    assert!(b.get("toolConfig").is_none());
    for (c, want) in [
        (ToolChoice::Auto, json!({"mode":"AUTO"})),
        (ToolChoice::None, json!({"mode":"NONE"})),
        (ToolChoice::Required, json!({"mode":"ANY"})),
        (
            ToolChoice::Function("a".into()),
            json!({"mode":"ANY","allowedFunctionNames":["a"]}),
        ),
    ] {
        r.tool_choice = Some(c);
        assert_eq!(body(&r)["toolConfig"]["functionCallingConfig"], want);
    }
}

#[test]
fn tool_round_trip_maps_ids_to_names_and_merges_responses() {
    let sig = "c2ln";
    let id_a = make_call_id(1, Some(sig));
    let id_b = make_call_id(2, None);
    let mut asst = Message::text(MessageRole::Assistant, "");
    asst.content = Value::Null;
    asst.tool_calls = vec![
        ToolCall {
            id: id_a.clone(),
            name: "a".into(),
            arguments: "{\"x\":1}".into(),
            provider_extra: None,
        },
        ToolCall {
            id: id_b.clone(),
            name: "b".into(),
            arguments: "{}".into(),
            provider_extra: None,
        },
    ];
    let mut r = req();
    r.messages = vec![
        Message::text(MessageRole::User, "go"),
        asst,
        Message::tool_result(&id_a, "{\"temp\":3}"),
        Message::tool_result(&id_b, "plain text"),
    ];
    let b = body(&r);
    let c = b["contents"].as_array().unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[1]["role"], "model");
    assert_eq!(
        c[1]["parts"][0]["functionCall"],
        json!({"name":"a","args":{"x":1}})
    );
    assert_eq!(c[1]["parts"][0]["thoughtSignature"], sig);
    assert!(c[1]["parts"][1].get("thoughtSignature").is_none());
    assert_eq!(c[2]["role"], "user");
    let rp = c[2]["parts"].as_array().unwrap();
    assert_eq!(rp.len(), 2);
    assert_eq!(
        rp[0]["functionResponse"],
        json!({"name":"a","response":{"temp":3}})
    );
    assert_eq!(
        rp[1]["functionResponse"],
        json!({"name":"b","response":{"output":"plain text"}})
    );

    r.messages.push(Message::tool_result("unknown", "x"));
    assert!(matches!(
        build_body(&entry("http://x"), &r),
        Err(ProviderError::InvalidRequest(_))
    ));
}

#[test]
fn images_become_inline_data_and_plain_urls_are_rejected() {
    let mut r = req();
    r.messages = vec![Message {
        role: MessageRole::User,
        content: json!([
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,QUJD"}}
        ]),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];
    assert_eq!(
        body(&r)["contents"][0]["parts"][1],
        json!({"inlineData":{"mimeType":"image/jpeg","data":"QUJD"}})
    );
    r.messages[0].content = json!([{"type":"image_url","image_url":{"url":"https://x/a.png"}}]);
    assert!(matches!(
        build_body(&entry("http://x"), &r),
        Err(ProviderError::InvalidRequest(_))
    ));
    let mut e = entry("http://x");
    e.capabilities.vision = false;
    r.messages[0].content =
        json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}]);
    assert!(build_body(&e, &r).is_err());
}

// ---- schema conversion ----

#[test]
fn schema_conversion_strips_and_rewrites() {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Args",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {"type": "string", "description": "file", "format": "uri", "default": "x"},
            "count": {"type": ["integer", "null"], "minimum": 0, "exclusiveMaximum": 10, "format": "int32"},
            "mode": {"const": "fast"},
            "kind": {"enum": ["a", "b"]},
            "level": {"type": "integer", "enum": [1, 2]},
            "when": {"type": "string", "format": "date-time"},
            "tags": {"type": "array", "items": {"type": "string"}, "uniqueItems": true, "maxItems": 3},
            "either": {"oneOf": [{"type": "string"}, {"type": "number"}]},
            "multi": {"type": ["string", "number"]},
            "node": {"$ref": "#/$defs/Node"},
            "ro": {"type": "string", "readOnly": true, "pattern": "^a"}
        },
        "required": ["path", "ghost"],
        "$defs": {"Node": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}}
    });
    let out = convert_parameters(&schema).unwrap();
    let s = out.to_string();
    for banned in [
        "$schema",
        "title",
        "additionalProperties",
        "default",
        "exclusiveMaximum",
        "uniqueItems",
        "readOnly",
        "\"uri\"",
        "$defs",
        "$ref",
        "const",
        "oneOf",
    ] {
        assert!(!s.contains(banned), "{banned} leaked: {s}");
    }
    assert_eq!(out["type"], "OBJECT");
    assert_eq!(out["required"], json!(["path"]));
    let p = &out["properties"];
    assert_eq!(p["path"], json!({"type":"STRING","description":"file"}));
    assert_eq!(
        p["count"],
        json!({"type":"INTEGER","nullable":true,"minimum":0,"format":"int32"})
    );
    assert_eq!(p["mode"], json!({"type":"STRING","enum":["fast"]}));
    assert_eq!(p["kind"], json!({"type":"STRING","enum":["a","b"]}));
    assert_eq!(p["level"], json!({"type":"STRING","enum":["1","2"]}));
    assert_eq!(p["when"], json!({"type":"STRING","format":"date-time"}));
    assert_eq!(
        p["tags"],
        json!({"type":"ARRAY","items":{"type":"STRING"},"maxItems":3})
    );
    assert_eq!(
        p["either"],
        json!({"anyOf":[{"type":"STRING"},{"type":"NUMBER"}]})
    );
    assert_eq!(
        p["multi"],
        json!({"anyOf":[{"type":"STRING"},{"type":"NUMBER"}]})
    );
    assert_eq!(p["node"]["properties"]["id"]["type"], "STRING");
    assert_eq!(p["node"]["required"], json!(["id"]));
    assert_eq!(p["ro"], json!({"type":"STRING","pattern":"^a"}));
}

#[test]
fn schema_all_of_is_merged_and_recursion_is_bounded() {
    let schema = json!({
        "type": "object",
        "allOf": [
            {"properties": {"a": {"type": "string"}}, "required": ["a"]},
            {"properties": {"b": {"type": "boolean"}}}
        ],
        "properties": {"t": {"$ref": "#/$defs/Tree"}},
        "$defs": {"Tree": {"type": "object", "properties": {"kids": {"type": "array", "items": {"$ref": "#/$defs/Tree"}}}}}
    });
    let out = convert_parameters(&schema).unwrap();
    assert_eq!(out["properties"]["a"]["type"], "STRING");
    assert_eq!(out["properties"]["b"]["type"], "BOOLEAN");
    assert_eq!(out["required"], json!(["a"]));
    assert!(out["properties"]["t"]["properties"]["kids"]["items"].is_object());
}

#[test]
fn schema_without_properties_yields_none() {
    assert!(convert_parameters(&json!({"type":"object"})).is_none());
    assert!(convert_parameters(&json!({"type":"object","properties":{}})).is_none());
    assert!(convert_parameters(&json!(true)).is_none());
}

// ---- live ----

/// Live test (text stream + one tool call). Runs only with `GEMINI_API_KEY`
/// set; picks a Flash Lite model from `models.list` unless `GEMINI_TEST_MODEL`
/// is given. Retries 429s via the provider's retry-after handling.
#[tokio::test]
#[ignore = "needs GEMINI_API_KEY and network"]
async fn live_gemini_text_and_tool_call() {
    let Ok(key) = std::env::var("GEMINI_API_KEY") else {
        return;
    };
    let mut e = entry("https://generativelanguage.googleapis.com/v1beta");
    e.auth = Auth::ApiKeyEnv("GEMINI_API_KEY".into());
    let p = GeminiProvider::with_api_key(e, OpenAiCompatConfig::default(), Some(key)).unwrap();
    let model = if let Ok(m) = std::env::var("GEMINI_TEST_MODEL") {
        m
    } else {
        let models = p.list_models().await.unwrap();
        models
            .iter()
            .find(|m| m.contains("flash-lite") && !m.contains("tts"))
            .cloned()
            .expect("no flash-lite model listed")
    };

    let mut r = ChatRequest::new(
        ModelRef(format!("gemini/{model}")),
        vec![Message::text(
            MessageRole::User,
            "Reply with the single word: pong",
        )],
    );
    r.max_tokens = Some(256);
    let chunks = collect(&p, r).await.unwrap();
    assert!(chunks.iter().any(|c| matches!(c, ChatChunk::TextDelta(_))));
    assert!(chunks.iter().any(|c| matches!(c, ChatChunk::Done { .. })));

    let mut r = ChatRequest::new(
        ModelRef(format!("gemini/{model}")),
        vec![Message::text(
            MessageRole::User,
            "What is the weather in Paris? Use the tool.",
        )],
    );
    r.tools = vec![tool_def(
        "get_weather",
        &json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
    )];
    r.max_tokens = Some(256);
    let chunks = collect(&p, r.clone()).await.unwrap();
    let calls = tool_calls(&chunks);
    assert_eq!(calls.len(), 1, "{chunks:?}");
    assert_eq!(calls[0].name, "get_weather");

    // Replay the call with its result (exercises id -> name and signatures).
    let mut asst = Message::text(MessageRole::Assistant, "");
    asst.content = Value::Null;
    asst.tool_calls = calls.clone();
    r.messages.push(asst);
    r.messages
        .push(Message::tool_result(&calls[0].id, "{\"temp_c\":18}"));
    let chunks = collect(&p, r).await.unwrap();
    assert!(chunks.iter().any(|c| matches!(c, ChatChunk::Done { .. })));
}

#[test]
fn huge_retry_delay_does_not_panic() {
    let body = |d: &str| {
        gerr(
            429,
            "RESOURCE_EXHAUSTED",
            "quota",
            json!([{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":d}]),
        )
        .to_string()
    };
    for d in ["1e30s", "1e308s", "inf", "infs", "NaNs", "-5s"] {
        let r = super::stream::retry_delay(&body(d));
        assert!(r.is_none_or(|x| x <= Duration::from_secs(24 * 3600)), "{d}");
    }
    assert_eq!(
        super::stream::retry_delay(&body("1e30s")),
        Some(Duration::from_secs(24 * 3600))
    );
    assert_eq!(
        super::stream::retry_delay(&body("1.5s")),
        Some(Duration::from_millis(1500))
    );
}
