use base64::Engine;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

use super::*;
use crate::{
    Auth, CompatFlags, ModelCapabilities, OpenAiCompatConfig, ProviderEntry, ProviderKind,
    build_image_provider, provider::ModelRef,
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

fn openai_request() -> ImageGenerateRequest {
    ImageGenerateRequest {
        model: ModelRef("test/gpt-image-1".into()),
        prompt: "a red circle".into(),
        size: Some("1024x1024".into()),
        format: ImageWireFormat::OpenAiImages,
    }
}

#[tokio::test]
async fn openai_returns_decoded_png_bytes() {
    let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({
            "model": "gpt-image-1",
            "prompt": "a red circle",
            "response_format": "b64_json",
            "size": "1024x1024",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"b64_json": b64}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider = build_image_provider(
        &entry(&server.uri(), ProviderKind::OpenAiCompatible),
        OpenAiCompatConfig::default(),
        Some("test-key".into()),
    )
    .unwrap();
    let out = provider.generate(openai_request()).await.unwrap();
    assert_eq!(out.bytes, png);
    assert_eq!(out.mime_type, "image/png");
}
