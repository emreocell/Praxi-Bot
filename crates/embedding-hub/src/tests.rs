//! Hermetic tests with fake provider servers (no network, no real keys).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::config::{EmbeddingConfig, ProviderConfig, ProviderType, Purpose, VectorSpace};
use crate::hub::{EmbedItem, EmbeddingHub, Outcome, Redactor, Task};
use crate::limiter::Limits;

const SECRET: &str = "sk-CANARY-1234567890abcdef";

fn provider(id: &str, kind: ProviderType, url: &str, dims: usize) -> ProviderConfig {
    ProviderConfig {
        id: id.to_owned(),
        kind,
        base_url: Some(url.to_owned()),
        model: match kind {
            ProviderType::Gemini => "gemini-embedding-001".to_owned(),
            _ => "text-embedding-3-small".to_owned(),
        },
        dimensions: dims,
        request_dimensions: true,
        key_ref: Some("embedding-key".to_owned()),
        allow_remote: true,
        limits: None,
        allow_local_network: false,
    }
}

#[allow(clippy::unnecessary_wraps)]
fn keys(_: &str) -> Option<String> {
    Some("test-key".to_owned())
}

fn hub_with(providers: Vec<ProviderConfig>, chain: &[&str]) -> EmbeddingHub {
    let mut config = EmbeddingConfig {
        providers,
        ..EmbeddingConfig::default()
    };
    for p in Purpose::ALL {
        config
            .chains
            .insert(p, chain.iter().map(|s| (*s).to_owned()).collect());
    }
    let redact: Redactor = Arc::new(|t| t.replace(SECRET, "[redacted]"));
    EmbeddingHub::new(config, &keys, Some(redact)).unwrap()
}

async fn embed(hub: &EmbeddingHub, texts: &[&str]) -> crate::hub::Embedded {
    let items: Vec<EmbedItem> = texts.iter().map(|t| EmbedItem::new(*t)).collect();
    hub.embed(
        Purpose::Memory,
        &items,
        Task::Document,
        None,
        None,
        Duration::ZERO,
    )
    .await
    .unwrap()
}

fn openai_body(n: usize, dims: usize) -> Value {
    json!({
        "data": (0..n).map(|i| json!({"index": i, "embedding": vec![0.5; dims]})).collect::<Vec<_>>(),
        "usage": {"prompt_tokens": 3}
    })
}

async fn openai_server(status: u16, body: Value, retry_after: Option<&str>) -> MockServer {
    let server = MockServer::start().await;
    let mut resp = ResponseTemplate::new(status).set_body_json(body);
    if let Some(r) = retry_after {
        resp = resp.insert_header("retry-after", r);
    }
    Mock::given(method("POST"))
        .and(path_regex("/embeddings$"))
        .respond_with(resp)
        .mount(&server)
        .await;
    server
}

fn attempt(e: &crate::hub::Embedded, i: usize) -> Outcome {
    e.attempts[i].outcome
}

#[tokio::test]
async fn no_providers_means_offline_hash_and_nothing_is_sent() {
    let hub = hub_with(Vec::new(), &[]);
    let e = embed(&hub, &["I live in Berlin"]).await;
    assert_eq!(e.space, VectorSpace::hash());
    assert_eq!(e.vectors[0].len(), 384);
}

#[tokio::test]
async fn openai_compatible_success_and_dimension_check() {
    let server = openai_server(200, openai_body(2, 8), None).await;
    let hub = hub_with(
        vec![provider(
            "my-endpoint",
            ProviderType::Compatible,
            &server.uri(),
            8,
        )],
        &["my-endpoint"],
    );
    let e = embed(&hub, &["a", "b"]).await;
    assert_eq!(e.space.provider, "my-endpoint");
    assert_eq!(e.vectors.len(), 2);

    // The server answers with 4 numbers but 8 were configured: falls to the hash.
    let server = openai_server(200, openai_body(1, 4), None).await;
    let hub = hub_with(
        vec![provider(
            "bad-size",
            ProviderType::Compatible,
            &server.uri(),
            8,
        )],
        &["bad-size"],
    );
    let e = embed(&hub, &["a"]).await;
    assert_eq!(attempt(&e, 0), Outcome::DimensionMismatch);
    assert_eq!(e.space, VectorSpace::hash());
}

#[tokio::test]
async fn gemini_success_and_key_rejected_falls_back_to_next_provider() {
    let gemini = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(":batchEmbedContents$"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"code": 401, "message": "bad key"}})),
        )
        .mount(&gemini)
        .await;
    let openai = openai_server(200, openai_body(1, 8), None).await;
    let hub = hub_with(
        vec![
            provider("gemini", ProviderType::Gemini, &gemini.uri(), 8),
            provider("openai", ProviderType::Openai, &openai.uri(), 8),
        ],
        &["gemini", "openai"],
    );
    let e = embed(&hub, &["hello"]).await;
    assert_eq!(attempt(&e, 0), Outcome::KeyRejected);
    assert_eq!(e.space.provider, "openai");
    // The rejected key rests: a second call does not hit Gemini again.
    let e = embed(&hub, &["hello"]).await;
    assert_eq!(attempt(&e, 0), Outcome::CoolingDown);
    let st = hub.status();
    assert_eq!(st[0].state, Outcome::KeyRejected);
    assert!(st[0].retry_in_secs > 500);

    let ok = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(":batchEmbedContents$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"embeddings": [{"values": vec![0.25; 8]}]})),
        )
        .mount(&ok)
        .await;
    let hub = hub_with(
        vec![provider("gemini", ProviderType::Gemini, &ok.uri(), 8)],
        &["gemini"],
    );
    assert_eq!(embed(&hub, &["hi"]).await.space.provider, "gemini");
}

#[tokio::test]
async fn rate_limit_with_retry_after_cools_down_and_degrades() {
    let server = openai_server(429, json!({"error": {"message": "slow down"}}), Some("120")).await;
    let hub = hub_with(
        vec![provider("o", ProviderType::Openai, &server.uri(), 8)],
        &["o"],
    );
    let e = embed(&hub, &["x"]).await;
    assert_eq!(attempt(&e, 0), Outcome::RateLimited);
    assert_eq!(e.space, VectorSpace::hash());
    let st = hub.status();
    assert_eq!(st[0].state, Outcome::RateLimited);
    assert!((100..=120).contains(&st[0].retry_in_secs));
    // Exactly one request reached the server: the second call did not retry.
    embed(&hub, &["x"]).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn limiter_degrades_instead_of_calling_when_the_free_quota_is_used() {
    let server = openai_server(200, openai_body(1, 8), None).await;
    let mut p = provider("o", ProviderType::Openai, &server.uri(), 8);
    p.limits = Some(Limits {
        rpm: Some(2),
        tpm: None,
        rpd: None,
    });
    let hub = hub_with(vec![p], &["o"]);
    // rpm 2 -> 90% margin = 1 request per minute.
    assert_eq!(embed(&hub, &["a"]).await.space.provider, "o");
    let e = embed(&hub, &["a"]).await;
    assert_eq!(attempt(&e, 0), Outcome::WaitTooLong);
    assert_eq!(e.space, VectorSpace::hash());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let bars = hub.status()[0].usage;
    assert_eq!(bars.requests_per_minute.used, 1);
    assert_eq!(bars.requests_per_minute.limit, Some(2));
}

#[tokio::test]
async fn nothing_is_sent_without_opt_in_for_private_text_or_denied_bots() {
    let server = openai_server(200, openai_body(1, 8), None).await;
    let mut p = provider("o", ProviderType::Openai, &server.uri(), 8);
    p.allow_remote = false;
    let hub = hub_with(vec![p.clone()], &["o"]);
    let e = embed(&hub, &["secret plans"]).await;
    assert_eq!(attempt(&e, 0), Outcome::NotAllowed);

    p.allow_remote = true;
    let hub = hub_with(vec![p], &["o"]);
    let private = [EmbedItem {
        text: "my diary".into(),
        private: true,
    }];
    let e = hub
        .embed(
            Purpose::Memory,
            &private,
            Task::Document,
            None,
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
    assert_eq!(e.space, VectorSpace::hash());

    let mut cfg = hub.config();
    cfg.remote_denied_bots.insert("bot-a".into());
    hub.reconfigure(cfg, &keys).unwrap();
    let e = hub
        .embed(
            Purpose::Memory,
            &[EmbedItem::new("note")],
            Task::Document,
            Some("bot-a"),
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
    assert_eq!(e.space, VectorSpace::hash());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn secrets_are_redacted_before_a_text_leaves_the_computer() {
    let server = openai_server(200, openai_body(1, 8), None).await;
    let hub = hub_with(
        vec![provider("o", ProviderType::Openai, &server.uri(), 8)],
        &["o"],
    );
    embed(&hub, &[&format!("my key is {SECRET} ok")]).await;
    let sent =
        String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
    assert!(!sent.contains(SECRET), "{sent}");
    assert!(sent.contains("[redacted]"));
}

#[tokio::test]
async fn pinned_space_is_never_answered_by_another_provider() {
    let server = openai_server(429, json!({}), None).await;
    let hub = hub_with(
        vec![provider("o", ProviderType::Openai, &server.uri(), 8)],
        &["o"],
    );
    let pin = hub.preferred_space(Purpose::Router);
    assert_eq!(pin.provider, "o");
    let r = hub
        .embed(
            Purpose::Router,
            &[EmbedItem::new("q")],
            Task::Query,
            None,
            Some(&pin),
            Duration::ZERO,
        )
        .await;
    assert_eq!(r.unwrap_err().attempts[0].outcome, Outcome::RateLimited);
    // After the failure the preferred space moves to the hash.
    assert_eq!(hub.preferred_space(Purpose::Router), VectorSpace::hash());
}

#[tokio::test]
async fn test_connection_uses_a_fixed_sentence_and_needs_the_opt_in() {
    let server = openai_server(200, openai_body(1, 8), None).await;
    let mut p = provider("o", ProviderType::Openai, &server.uri(), 8);
    p.allow_remote = false;
    let hub = hub_with(vec![p.clone()], &["o"]);
    assert_eq!(hub.test("o").await, Outcome::NotAllowed);
    p.allow_remote = true;
    let hub = hub_with(vec![p], &["o"]);
    assert_eq!(hub.test("o").await, Outcome::Ok);
    assert_eq!(hub.test("missing").await, Outcome::Unavailable);
}

#[test]
fn config_validation_is_plain_and_round_trips() {
    let hub = hub_with(Vec::new(), &[]);
    let mut cfg = EmbeddingConfig::default();
    let mut p = provider(
        "x",
        ProviderType::Compatible,
        "http://localhost:11434/v1",
        768,
    );
    p.base_url = None;
    cfg.providers.push(p.clone());
    assert!(
        hub.reconfigure(cfg.clone(), &keys)
            .unwrap_err()
            .0
            .contains("endpoint")
    );
    p.base_url = Some("http://user:pw@host/v1".into());
    cfg.providers[0] = p.clone();
    assert!(hub.reconfigure(cfg.clone(), &keys).is_err());
    p.base_url = Some("http://localhost:11434/v1".into());
    p.dimensions = 99_999;
    cfg.providers[0] = p;
    assert!(hub.reconfigure(cfg.clone(), &keys).is_err());
    cfg.providers[0].dimensions = 768;
    cfg.chains.insert(Purpose::Memory, vec!["nope".into()]);
    assert!(
        hub.reconfigure(cfg.clone(), &keys)
            .unwrap_err()
            .0
            .contains("unknown")
    );
    cfg.chains.clear();
    hub.reconfigure(cfg.clone(), &keys).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("embeddings.json");
    cfg.save(&file).unwrap();
    assert_eq!(EmbeddingConfig::load(&file).unwrap(), cfg);
    assert_eq!(
        EmbeddingConfig::load(&dir.path().join("none.json")).unwrap(),
        EmbeddingConfig::default()
    );
    // The file never holds a key.
    assert!(!std::fs::read_to_string(&file).unwrap().contains("test-key"));
}

#[tokio::test]
async fn without_a_redactor_remote_providers_are_never_called() {
    let server = openai_server(200, openai_body(1, 8), None).await;
    let hub = hub_with(
        vec![provider("o", ProviderType::Openai, &server.uri(), 8)],
        &["o"],
    );
    hub.set_redactor(None);
    let e = embed(&hub, &["x"]).await;
    assert_eq!(attempt(&e, 0), Outcome::NotAllowed);
    assert!(server.received_requests().await.unwrap().is_empty());
}
