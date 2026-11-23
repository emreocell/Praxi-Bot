use std::time::Duration;

use futures::StreamExt;
use provider_catalog::{
    Auth, ChatChunk, ChatProvider, ChatRequest, CompatFlags, FailureClass, ModelCapabilities,
    ModelRef, OpenAiCompatConfig, ProviderEntry, ProviderKind, build_chat_provider,
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[derive(Clone, Copy, Debug)]
enum Adapter {
    OpenAi,
    LlamaServer,
    Anthropic,
    Gemini,
}

impl Adapter {
    const ALL: [Self; 4] = [
        Self::OpenAi,
        Self::LlamaServer,
        Self::Anthropic,
        Self::Gemini,
    ];

    fn build(self, base_url: String, config: OpenAiCompatConfig) -> Box<dyn ChatProvider> {
        let kind = match self {
            Self::OpenAi | Self::LlamaServer => ProviderKind::OpenAiCompatible,
            Self::Anthropic => ProviderKind::Anthropic,
            Self::Gemini => ProviderKind::Gemini,
        };
        let entry = ProviderEntry {
            id: if matches!(self, Self::LlamaServer) {
                "llamacpp"
            } else {
                "fake"
            }
            .into(),
            kind,
            base_url,
            auth: Auth::None,
            capabilities: ModelCapabilities {
                streaming: true,
                tools: true,
                vision: false,
                json_mode: false,
            },
            compat_flags: CompatFlags::default(),
            models_endpoint: None,
            models: Vec::new(),
        };
        build_chat_provider(&entry, config, None).unwrap()
    }

    fn usage_and_error(self) -> String {
        let (usage, error) = match self {
            Self::OpenAi | Self::LlamaServer => (
                json!({"usage":{"prompt_tokens":19,"completion_tokens":2}}),
                json!({"error":{"code":"rate_limit_exceeded","message":"slow down"}}),
            ),
            Self::Anthropic => (
                json!({"type":"message_start","message":{"usage":{"input_tokens":19,"output_tokens":2}}}),
                json!({"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}),
            ),
            Self::Gemini => (
                json!({"usageMetadata":{"promptTokenCount":19,"candidatesTokenCount":2}}),
                json!({"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"slow down"}}),
            ),
        };
        format!("data: {usage}\n\ndata: {error}\n\n")
    }

    fn unfinished_tool(self) -> String {
        let frames = match self {
            Self::OpenAi | Self::LlamaServer => vec![json!({"choices":[{"delta":{
                "tool_calls":[{"index":0,"id":"cut","function":{"name":"write","arguments":"{\"path\":"}}]
            }}]})],
            Self::Anthropic => vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"cut","name":"write"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            ],
            Self::Gemini => vec![json!({"candidates":[{"content":{"parts":[{"functionCall":{
                "name":"write","args":{"path":"report.txt"}
            }}]}}]})],
        };
        frames
            .into_iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect()
    }

    fn text_frame(self) -> String {
        let text = match self {
            Self::OpenAi | Self::LlamaServer => json!({
                "choices":[{"delta":{"content":"part"}}],
                "usage":{"prompt_tokens":19,"completion_tokens":2}
            }),
            Self::Anthropic => json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"part"}}),
            Self::Gemini => json!({"candidates":[{"content":{"parts":[{"text":"part"}]}}],
                "usageMetadata":{"promptTokenCount":19,"candidatesTokenCount":2}}),
        };
        let usage = if matches!(self, Self::Anthropic) {
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":19,\"output_tokens\":2}}}\n\n"
        } else {
            ""
        };
        format!("{usage}data: {text}\n\n")
    }
}

fn config() -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        max_retries: 0,
        timeout: Duration::from_secs(1),
        ..OpenAiCompatConfig::default()
    }
}

fn request() -> ChatRequest {
    ChatRequest::new(ModelRef("test-model".into()), vec![])
}

#[tokio::test]
async fn adapters_preserve_usage_on_stream_errors_and_classify_the_failure() {
    for adapter in Adapter::ALL {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(adapter.usage_and_error(), "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let stream = adapter
            .build(server.uri(), config())
            .stream(request())
            .await
            .unwrap();
        let chunks = stream.collect::<Vec<_>>().await;
        assert!(
            matches!(chunks.as_slice(), [Ok(ChatChunk::Usage(u)), Err(e)]
                if u.prompt_tokens == 19 && u.completion_tokens == 2
                    && e.class() == FailureClass::RateLimit),
            "{adapter:?}: {chunks:?}"
        );
    }
}

#[tokio::test]
async fn adapters_classify_context_limit_before_retrying() {
    for adapter in Adapter::ALL {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "This request exceeds the model's context length"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let error = adapter
            .build(server.uri(), config())
            .stream(request())
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.class(),
            FailureClass::ContextTooLong,
            "{adapter:?}: {error}"
        );
    }
}

#[tokio::test]
async fn one_attempt_preserves_failure_classes_and_server_retry_delays() {
    let retry_date =
        httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(600));
    for adapter in Adapter::ALL {
        for (status, code, class, retryable) in [
            (401, "authentication_error", FailureClass::Auth, false),
            (429, "insufficient_quota", FailureClass::Quota, false),
            (503, "overloaded_error", FailureClass::Overloaded, true),
            (400, "content_filter", FailureClass::ContentFiltered, false),
            (504, "gateway_timeout", FailureClass::Timeout, true),
        ] {
            let server = MockServer::start().await;
            // A zero delay would let the ordinary streaming entry point retry.
            let retry_after = if status == 503 {
                "0"
            } else {
                retry_date.as_str()
            };
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Retry-After", retry_after)
                        .set_body_json(json!({"error":{"type":code,"message":"fixture"}})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let p = adapter.build(server.uri(), OpenAiCompatConfig::default());
            let error = p.stream_once(request()).await.err().unwrap();
            assert_eq!(error.class(), class, "{adapter:?}: {error}");
            assert_eq!(error.is_retryable(), retryable);
            if status == 503 {
                assert_eq!(error.retry_after(), Some(Duration::ZERO));
            } else if retryable {
                assert!(error.retry_after().unwrap() > Duration::from_secs(540));
            } else {
                assert!(error.retry_after().is_none());
            }
        }
    }
}

#[tokio::test]
async fn broken_stream_cannot_promote_partial_tools_or_hide_protocol_failure() {
    for adapter in Adapter::ALL {
        for (body, class) in [
            (adapter.unfinished_tool(), FailureClass::Network),
            ("data: {bad json}\n\n".into(), FailureClass::Protocol),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
                .expect(1)
                .mount(&server)
                .await;
            let stream = adapter
                .build(server.uri(), config())
                .stream_once(request())
                .await
                .unwrap();
            let chunks = stream.collect::<Vec<_>>().await;
            assert!(
                !chunks
                    .iter()
                    .any(|c| matches!(c, Ok(ChatChunk::ToolCall(_) | ChatChunk::Done { .. })))
            );
            assert!(
                matches!(chunks.last(), Some(Err(e)) if e.class() == class),
                "{adapter:?}: {chunks:?}"
            );
        }
    }
}

#[tokio::test]
async fn gemini_classifies_an_invalid_key_in_a_successful_http_stream() {
    let server = MockServer::start().await;
    let error = json!({"error":{
        "code":400, "status":"INVALID_ARGUMENT", "message":"credential rejected",
        "details":[{"reason":"API_KEY_INVALID"}]
    }});
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("data: {error}\n\n"), "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let p = Adapter::Gemini.build(server.uri(), config());
    let mut stream = p.stream_once(request()).await.unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().class(),
        FailureClass::Auth
    );
}

#[tokio::test]
async fn idle_stream_times_out_despite_keepalives_and_releases_the_connection() {
    for adapter in Adapter::ALL {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(tcp);
            loop {
                let mut line = String::new();
                assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            let (mut reader, mut writer) = socket.into_inner().into_split();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 999999\r\n\r\n{}",
                adapter.text_frame(),
            );
            writer.write_all(response.as_bytes()).await.unwrap();
            let mut discarded = Vec::new();
            tokio::select! {
                _ = reader.read_to_end(&mut discarded) => {}
                () = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(15)).await;
                        if writer.write_all(b": keepalive\n\n").await.is_err() {
                            return;
                        }
                    }
                } => {}
            }
        });
        let p = adapter.build(
            url,
            OpenAiCompatConfig {
                timeout: Duration::from_millis(250),
                ..config()
            },
        );
        let mut stream = p.stream_once(request()).await.unwrap();
        let chunks =
            tokio::time::timeout(Duration::from_secs(3), stream.by_ref().collect::<Vec<_>>())
                .await
                .expect("keepalives must not keep the model response alive");
        assert!(
            matches!(chunks.as_slice(),
            [Ok(ChatChunk::TextDelta(text)), Ok(ChatChunk::Usage(usage)), Err(error)]
                if text == "part" && usage.prompt_tokens == 19 && error.class() == FailureClass::Timeout),
            "{adapter:?}: {chunks:?}"
        );
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .expect("the adapter must close its response")
            .unwrap();
    }
}
