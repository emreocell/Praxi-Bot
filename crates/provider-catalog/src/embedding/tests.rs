use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

use super::*;
use crate::{
    Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind, build_embedding_provider,
};

fn entry(base: &str, kind: ProviderKind) -> ProviderEntry {
    ProviderEntry {
        id: "test".into(),
        kind,
        base_url: base.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: false,
            vision: false,
            streaming: false,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    }
}

fn request(format: EmbeddingTaskFormat) -> EmbeddingRequest {
    EmbeddingRequest {
        model: ModelRef("test/embedding-model".into()),
        inputs: vec![
            EmbeddingInput {
                text: "first".into(),
                title: Some("Title".into()),
            },
            EmbeddingInput {
                text: "second".into(),
                title: None,
            },
        ],
        dimensions: 2,
        request_dimensions: true,
        task: EmbeddingTask::Document,
        format,
    }
}

fn openai_response(data: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({"model":"embedding-model","data":data,"usage":{"prompt_tokens":7}}))
}

#[tokio::test]
async fn openai_batches_reorders_indices_and_reuses_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/embeddings")).and(header("authorization","Bearer test-key"))
        .and(body_json(json!({"model":"embedding-model","input":["first","second"],"dimensions":2,"encoding_format":"float"})))
        .respond_with(openai_response(&json!([{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}])))
        .expect(1).mount(&server).await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        Some("test-key".into()),
    )
    .unwrap();
    let response = provider
        .embed(request(EmbeddingTaskFormat::Plain))
        .await
        .unwrap();
    assert_eq!(response.vectors, vec![vec![1., 0.], vec![0., 1.]]);
    assert_eq!(response.input_tokens, Some(7));
}

#[tokio::test]
async fn gemini_001_preserves_one_request_per_document_and_task_titles() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/models/embedding-model:batchEmbedContents"))
        .and(header("x-goog-api-key","test-key"))
        .and(body_json(json!({"requests":[
            {"model":"models/embedding-model","content":{"parts":[{"text":"first"}]},"taskType":"RETRIEVAL_DOCUMENT","title":"Title","outputDimensionality":2},
            {"model":"models/embedding-model","content":{"parts":[{"text":"second"}]},"taskType":"RETRIEVAL_DOCUMENT","outputDimensionality":2}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"embeddings":[{"values":[1.,0.]},{"values":[0.,1.]}]})))
        .expect(1).mount(&server).await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::Gemini),
        OpenAiCompatConfig::default(),
        Some("test-key".into()),
    )
    .unwrap();
    assert_eq!(
        provider
            .embed(request(EmbeddingTaskFormat::Gemini001))
            .await
            .unwrap()
            .vectors
            .len(),
        2
    );
}

#[tokio::test]
async fn gemini_2_prefixes_each_query_without_task_type_or_aggregation() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/models/embedding-model:batchEmbedContents"))
        .and(body_json(json!({"requests":[
            {"model":"models/embedding-model","content":{"parts":[{"text":"task: search result | query: first"}]},"outputDimensionality":2},
            {"model":"models/embedding-model","content":{"parts":[{"text":"task: search result | query: second"}]},"outputDimensionality":2}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"embeddings":[{"values":[1.,0.]},{"values":[0.,1.]}]})))
        .expect(1).mount(&server).await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::Gemini),
        OpenAiCompatConfig::default(),
        None,
    )
    .unwrap();
    let mut request = request(EmbeddingTaskFormat::Gemini2);
    request.task = EmbeddingTask::Query;
    assert_eq!(provider.embed(request).await.unwrap().vectors.len(), 2);
}

#[tokio::test]
async fn malformed_vectors_indices_and_counts_are_rejected_but_gateway_alias_is_preserved() {
    let invalid_data = [
        json!([{"index":0,"embedding":[1.,0.]}]),
        json!([{"index":0,"embedding":[1.,0.]},{"index":0,"embedding":[0.,1.]}]),
        json!([{"index":0,"embedding":[1.,0.]},{"index":2,"embedding":[0.,1.]}]),
        json!([{"index":0,"embedding":[1.]},{"index":1,"embedding":[0.,1.]}]),
        json!([{"index":0,"embedding":[1e300,0.]},{"index":1,"embedding":[0.,1.]}]),
    ];
    for data in invalid_data {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(openai_response(&data))
            .mount(&server)
            .await;
        let provider = build_embedding_provider(
            &entry(&server.uri(), ProviderKind::OpenAiCompatible),
            OpenAiCompatConfig::default(),
            None,
        )
        .unwrap();
        assert!(
            provider
                .embed(request(EmbeddingTaskFormat::Plain))
                .await
                .is_err()
        );
    }
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"model":"different-model","data":[{"index":0,"embedding":[1.,0.]},{"index":1,"embedding":[0.,1.]}]}))).mount(&server).await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        None,
    )
    .unwrap();
    let response = provider
        .embed(request(EmbeddingTaskFormat::Plain))
        .await
        .unwrap();
    assert_eq!(response.model.as_deref(), Some("different-model"));
}

#[tokio::test]
async fn retries_belong_to_durable_worker_and_errors_do_not_echo_private_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "900")
                .set_body_json(json!({"error":{"message":"private-document-secret"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        None,
    )
    .unwrap();
    let error = provider
        .embed(request(EmbeddingTaskFormat::Plain))
        .await
        .unwrap_err();
    assert!(
        matches!(error,ProviderError::RateLimited{retry_after:Some(duration)} if duration==Duration::from_secs(900))
    );
    assert!(!format!("{error}").contains("private-document-secret"));
}

#[tokio::test]
async fn redirects_never_forward_input_or_credentials() {
    let origin = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/stolen", destination.uri())),
        )
        .expect(1)
        .mount(&origin)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&destination)
        .await;
    let provider = build_embedding_provider(
        &entry(&origin.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        Some("secret".into()),
    )
    .unwrap();
    assert!(
        provider
            .embed(request(EmbeddingTaskFormat::Plain))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn omitted_dimensions_works_for_compatible_endpoints_without_that_capability() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_json(
            json!({"model":"embedding-model","input":["first","second"],"encoding_format":"float"}),
        ))
        .respond_with(openai_response(
            &json!([{"index":0,"embedding":[1.,0.]},{"index":1,"embedding":[0.,1.]}]),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        None,
    )
    .unwrap();
    let mut req = request(EmbeddingTaskFormat::Plain);
    req.request_dimensions = false;
    provider.embed(req).await.unwrap();
}

#[tokio::test]
async fn model_discovery_selects_embedding_method_instead_of_chat() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[
            {"name":"models/embed-next","supportedGenerationMethods":["embedContent"]},
            {"name":"models/chat-next","supportedGenerationMethods":["generateContent"]}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let provider = build_embedding_provider(
        &entry(&server.uri(), ProviderKind::Gemini),
        OpenAiCompatConfig::default(),
        None,
    )
    .unwrap();
    assert_eq!(
        provider.list_embedding_models().await.unwrap(),
        vec!["embed-next"]
    );
}

#[test]
fn endpoint_and_request_validation_are_secret_free() {
    assert_eq!(
        endpoint_identity("https://EXAMPLE.com:443/v1/").unwrap(),
        "https://example.com/v1"
    );
    for endpoint in [
        "https://secret@host/v1",
        "https://host/v1?key=secret",
        "https://host/#secret",
        "file:///private",
    ] {
        let error = endpoint_identity(endpoint).unwrap_err();
        assert!(!error.to_string().contains("secret@"));
    }
    let mut req = request(EmbeddingTaskFormat::Plain);
    req.inputs[0].text = "confidential".into();
    assert!(!format!("{req:?}").contains("confidential"));
    req.dimensions = MAX_EMBEDDING_DIMENSIONS + 1;
    assert!(req.validate().is_err());
}

#[tokio::test]
async fn embedding_retry_info_preserves_long_delays_and_retry_after_header_precedence() {
    for (retry_header, body_delay, minimum) in [
        (None, "172800s", Duration::from_secs(172_800)),
        (
            Some(httpdate::fmt_http_date(
                SystemTime::now() + Duration::from_secs(172_800),
            )),
            "2s",
            Duration::from_secs(172_790),
        ),
    ] {
        let server = MockServer::start().await;
        let mut response=ResponseTemplate::new(429).set_body_json(json!({"error":{"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":body_delay}]}}));
        if let Some(header) = retry_header {
            response = response.insert_header("retry-after", header);
        }
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let provider = build_embedding_provider(
            &entry(&server.uri(), ProviderKind::Gemini),
            OpenAiCompatConfig::default(),
            None,
        )
        .unwrap();
        let error = provider
            .embed(request(EmbeddingTaskFormat::Gemini001))
            .await
            .unwrap_err();
        let ProviderError::RateLimited {
            retry_after: Some(delay),
        } = error
        else {
            panic!("expected durable retry delay")
        };
        assert!(
            delay >= minimum,
            "requested delay shortened: {delay:?} < {minimum:?}"
        );
    }
}

#[test]
fn durable_retry_info_handles_fractional_invalid_and_overflowing_delays() {
    let body = |delay: &str| json!({"error":{"details":[{"retryDelay":delay}]}}).to_string();
    assert_eq!(retry_info(&body("1.5s")), Some(Duration::from_millis(1500)));
    assert_eq!(
        retry_info(&body("1e30s")),
        Some(Duration::from_secs(172_800))
    );
    for delay in ["NaNs", "infs", "-1s", "not-a-delay"] {
        assert_eq!(retry_info(&body(delay)), None);
    }
}

#[test]
fn custom_endpoints_refuse_metadata_private_and_plain_http() {
    for bad in [
        "http://169.254.169.254/latest",
        "https://169.254.169.254/",
        "https://[fe80::1]/",
        "http://example.com/v1",
        "https://10.0.0.5/v1",
        "https://192.168.1.2/v1",
        "https://127.0.0.1/v1",
        "ftp://example.com/",
        "https://user:pw@example.com/",
    ] {
        assert!(
            validate_endpoint(bad, false).is_err() || bad.contains("127.0.0.1"),
            "{bad}"
        );
    }
    // https to a loopback address literal is fine (it is loopback); plain http only to loopback.
    assert!(validate_endpoint("http://localhost:11434/v1", false).is_ok());
    assert!(validate_endpoint("http://127.0.0.1:11434/v1", false).is_ok());
    assert!(validate_endpoint("http://[::1]:11434/v1", false).is_ok());
    assert!(validate_endpoint("https://api.example.com/v1", false).is_ok());
    // The owner switch opens the LAN but never link-local/metadata.
    assert!(validate_endpoint("https://192.168.1.2/v1", true).is_ok());
    assert!(validate_endpoint("https://169.254.169.254/", true).is_err());
}

#[tokio::test]
async fn a_metadata_endpoint_never_receives_a_request() {
    let provider = build_embedding_provider(
        &entry("http://169.254.169.254/v1", ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        Some("k".into()),
    )
    .unwrap();
    let error = provider
        .embed(request(EmbeddingTaskFormat::Plain))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::InvalidRequest(_)),
        "{error:?}"
    );
}
