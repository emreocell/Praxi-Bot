//! Hermetic tests for every ladder level.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

// ---------- fakes ----------

/// First key contained in the (lowercased) text selects the vector.
struct FakeEmbedder {
    table: Vec<(&'static str, Vec<f32>)>,
}

#[async_trait]
impl Embedder for FakeEmbedder {
    async fn embed(&self, texts: &[String], _: EmbedTask) -> Result<Vec<Vec<f32>>, BackendError> {
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                self.table
                    .iter()
                    .find(|(k, _)| t.contains(k))
                    .map_or_else(|| vec![0.0, 0.0, 1.0], |(_, v)| v.clone())
            })
            .collect())
    }
}

fn fake_embedder() -> Arc<dyn Embedder> {
    Arc::new(FakeEmbedder {
        table: vec![
            ("overview", vec![0.7, -0.714, 0.0]), // cos ~0.70 with a report doc
            ("report", vec![1.0, 0.0, 0.0]),
            ("screenshot", vec![0.0, 1.0, 0.0]),
        ],
    })
}

struct FakeClassifier {
    answer: Result<String, BackendError>,
    delay: Duration,
    calls: std::sync::atomic::AtomicUsize,
}

impl FakeClassifier {
    fn answering(s: &str) -> Arc<Self> {
        Arc::new(Self {
            answer: Ok(s.to_owned()),
            delay: Duration::ZERO,
            calls: 0.into(),
        })
    }
}

#[async_trait]
impl Classifier for FakeClassifier {
    async fn classify(&self, _: &str, _: u32) -> Result<String, BackendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.answer.clone()
    }
}

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate::new(
            CandidateKind::Workflow,
            "daily_report",
            "Send the daily sales report",
        ),
        Candidate::new(
            CandidateKind::Workflow,
            "backup_photos",
            "Copy photos to the backup drive",
        )
        .with_param("folder", true),
        Candidate::new(
            CandidateKind::Tool,
            "take_screenshot",
            "Capture the screen to an image",
        ),
        Candidate::new(
            CandidateKind::Skill,
            "instagram_post",
            "Publish a post to Instagram",
        ),
    ]
}

fn base() -> LadderBuilder {
    Ladder::builder()
}

async fn decide(l: &Ladder, text: &str) -> Decision {
    l.decide(&RouteContext::new(text)).await
}

// ---------- K0 ----------

#[tokio::test]
async fn k0_slash_and_user_rules() {
    let rules: Vec<UserRule> = serde_json::from_str(
        r#"[
          {"pattern": "^(?i)chrome.?u ac$", "route": "command", "name": "open_app", "args": ["chrome"]},
          {"pattern": "^rapor (?P<day>\\w+)$", "route": "workflow", "name": "daily_report"}
        ]"#,
    )
    .unwrap();
    let l = base().rules(&rules).unwrap().build();

    let d = decide(&l, "/stop").await;
    assert_eq!(d.level, Level::K0);
    assert!(matches!(d.route, Route::Command(ref c) if c.name == "stop"));

    let d = decide(&l, "/workflow daily_report day=monday").await;
    assert!(
        matches!(d.route, Route::Workflow { ref name, ref params, .. }
            if name == "daily_report" && params["day"] == "monday")
    );

    // Turkish casing/diacritics: matched through the normalised text.
    let d = decide(&l, "CHROME'U AÇ").await;
    assert_eq!(d.level, Level::K0);
    assert!(matches!(d.route, Route::Command(ref c) if c.args == ["chrome"]));

    let d = decide(&l, "rapor Pazartesi").await;
    assert!(matches!(d.route, Route::Workflow { ref params, .. } if params["day"] == "Pazartesi"));
    assert_eq!(l.metrics().decided[0], 4);
}

#[test]
fn invalid_rule_is_rejected() {
    let bad = UserRule {
        pattern: "(".into(),
        action: RuleAction::ToolHint { tools: vec![] },
    };
    assert!(matches!(
        base().rules(&[bad]),
        Err(RouterError::InvalidRule(_))
    ));
}

// ---------- K1 ----------

fn cached_ladder(clock: &Arc<AtomicI64>, ttl: u64) -> Ladder {
    let c = Arc::clone(clock);
    let l = base()
        .cache(RouteCache::in_memory(i64::try_from(ttl).unwrap()).unwrap())
        .clock(Arc::new(move || c.load(Ordering::SeqCst)))
        .build();
    l.set_candidates(candidates());
    l
}

fn report_route() -> Route {
    Route::Workflow {
        name: "daily_report".into(),
        params: BTreeMap::new(),
        confidence: 0.95,
    }
}

#[tokio::test]
async fn k1_hit_ttl_and_turkish_normalisation() {
    let clock = Arc::new(AtomicI64::new(1_000));
    let l = cached_ladder(&clock, 100);
    l.remember_route("İstanbul raporunu GÖNDER!", &report_route())
        .unwrap();

    // different casing / punctuation / diacritics -> same key
    let d = decide(&l, "istanbul raporunu gonder").await;
    assert_eq!(d.level, Level::K1);
    assert_eq!(d.route, report_route());
    assert_eq!(d.reason, "intent cache hit");

    clock.store(1_099, Ordering::SeqCst);
    assert_eq!(
        decide(&l, "istanbul raporunu gonder").await.level,
        Level::K1
    );

    // expired
    clock.store(1_100, Ordering::SeqCst);
    let d = decide(&l, "istanbul raporunu gonder").await;
    assert_ne!(d.level, Level::K1);
    assert_eq!(l.metrics().decided[1], 2);
}

#[tokio::test]
async fn k1_invalidated_when_workflow_changes_or_is_removed() {
    let clock = Arc::new(AtomicI64::new(0));
    let l = cached_ladder(&clock, 1_000_000);
    l.remember_route("send it", &report_route()).unwrap();
    assert_eq!(decide(&l, "send it").await.level, Level::K1);

    // workflow edited -> new version -> stale
    let mut edited = candidates();
    edited[0] = edited[0].clone().with_version("2");
    l.set_candidates(edited);
    assert_ne!(decide(&l, "send it").await.level, Level::K1);
    assert_eq!(l.metrics().k1_stale, 1);

    // entry was removed on the stale hit; re-remember with the new version
    l.remember_route("send it", &report_route()).unwrap();
    assert_eq!(decide(&l, "send it").await.level, Level::K1);

    // explicit invalidation
    assert_eq!(l.invalidate_workflow("daily_report").unwrap(), 1);
    assert_ne!(decide(&l, "send it").await.level, Level::K1);

    // workflow deleted entirely
    l.remember_route("send it", &report_route()).unwrap();
    l.set_candidates(vec![]);
    assert_ne!(decide(&l, "send it").await.level, Level::K1);
}

#[test]
fn remember_route_validation() {
    let clock = Arc::new(AtomicI64::new(0));
    let l = cached_ladder(&clock, 10);
    assert!(l.remember_route("x", &Route::Agent).is_err());
    let unknown = Route::Workflow {
        name: "nope".into(),
        params: BTreeMap::new(),
        confidence: 1.0,
    };
    assert!(l.remember_route("x", &unknown).is_err());
    assert!(l.remember_route("   ", &report_route()).is_err());
    let tool = Route::ToolHint {
        tools: vec!["take_screenshot".into()],
    };
    assert!(l.remember_route("shot please", &tool).is_ok());
    // no cache configured
    assert!(base().build().remember_route("x", &tool).is_err());
}

#[test]
fn cache_purge_and_file_persistence() {
    let dir = std::env::temp_dir().join(format!("router-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cache.db");
    {
        let c = RouteCache::open(&file, 50).unwrap();
        c.put("a", &Route::Agent, None, 0).unwrap();
        c.put("b", &Route::Agent, None, 40).unwrap();
        assert_eq!(c.len().unwrap(), 2);
    }
    let c = RouteCache::open(&file, 50).unwrap();
    assert_eq!(c.purge_expired(60).unwrap(), 1);
    assert!(c.get("b", 60).unwrap().is_some());
    assert!(c.get("a", 60).unwrap().is_none());
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

// ---------- K2 ----------

fn k2_ladder() -> Ladder {
    let l = base().embedder(fake_embedder()).build();
    l.set_candidates(candidates());
    l
}

#[tokio::test]
async fn k2_routes_workflow_only_above_threshold() {
    let l = k2_ladder();
    l.refresh_embeddings().await.unwrap();

    let d = decide(&l, "send me the daily report").await;
    assert_eq!(d.level, Level::K2, "{d:?}");
    assert!(
        matches!(d.route, Route::Workflow { ref name, .. } if name == "daily_report"),
        "{d:?}"
    );
    assert!(d.confidence >= 0.9);

    // cosine ~0.70, no lexical overlap: score ~0.57 < 0.9 -> not routed, no
    // classifier -> Agent (K4).
    let d = decide(&l, "overview please").await;
    assert_eq!(d.route, Route::Agent);
    assert_eq!(d.level, Level::K4);
}

#[tokio::test]
async fn k2_workflow_with_required_params_is_not_routed() {
    let l = k2_ladder();
    l.set_candidates(vec![candidates().remove(1)]); // backup_photos needs `folder`
    let d = decide(&l, "backup photos to the backup drive").await;
    assert_eq!(d.route, Route::Agent);
}

#[tokio::test]
async fn k2_tool_hint_and_skill_hint() {
    let l = k2_ladder();
    l.refresh_embeddings().await.unwrap();
    let d = decide(&l, "take a screenshot").await;
    assert_eq!(d.level, Level::K2);
    assert_eq!(
        d.route,
        Route::ToolHint {
            tools: vec!["take_screenshot".into()]
        }
    );

    // BM25 only (no embeddings): a strong lexical match still yields a hint
    // only above hint_threshold; weak matches fall through.
    let l = base().build();
    l.set_candidates(candidates());
    let d = decide(&l, "instagram").await;
    assert_ne!(
        d.level,
        Level::K2,
        "weak lexical match must not decide: {d:?}"
    );
}

#[tokio::test]
async fn k2_ambiguous_workflows_are_not_routed() {
    let l = base().embedder(fake_embedder()).build();
    l.set_candidates(vec![
        Candidate::new(CandidateKind::Workflow, "report_a", "daily report"),
        Candidate::new(CandidateKind::Workflow, "report_b", "daily report"),
    ]);
    l.refresh_embeddings().await.unwrap();
    let d = decide(&l, "daily report").await;
    assert_eq!(d.route, Route::Agent, "{d:?}");
}

#[tokio::test]
async fn embedder_failure_degrades_to_bm25() {
    struct Broken;
    #[async_trait]
    impl Embedder for Broken {
        async fn embed(&self, _: &[String], _: EmbedTask) -> Result<Vec<Vec<f32>>, BackendError> {
            Err(BackendError::new("down"))
        }
    }
    let l = base().embedder(Arc::new(Broken)).build();
    l.set_candidates(candidates());
    assert!(l.refresh_embeddings().await.is_err());
    let d = decide(&l, "hello there").await;
    assert_eq!(d.route, Route::Agent);
}

// ---------- K3 ----------

fn k3_ladder(c: Arc<FakeClassifier>) -> Ladder {
    let l = base().classifier(c).build();
    l.set_candidates(candidates());
    l
}

#[tokio::test]
async fn k3_valid_answers() {
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"workflow","name":"backup_photos","params":{"folder":"D:/pics"},"confidence":0.95}"#,
    ));
    let d = decide(&l, "fotoğrafları yedekle").await;
    assert_eq!(d.level, Level::K3);
    assert!(
        matches!(d.route, Route::Workflow { ref name, ref params, .. }
        if name == "backup_photos" && params["folder"] == "D:/pics")
    );

    // fenced JSON, tool hint
    let l = k3_ladder(FakeClassifier::answering(
        "```json\n{\"route\":\"tool\",\"name\":\"take_screenshot\",\"params\":null,\"confidence\":0.7}\n```",
    ));
    let d = decide(&l, "grab the display").await;
    assert_eq!(
        d.route,
        Route::ToolHint {
            tools: vec!["take_screenshot".into()]
        }
    );

    // skill hint
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"skill","name":"instagram_post","confidence":0.8}"#,
    ));
    assert_eq!(
        decide(&l, "post this").await.route,
        Route::SkillHint {
            skills: vec!["instagram_post".into()]
        }
    );

    // classifier says agent
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"agent","name":null,"confidence":0.8}"#,
    ));
    let d = decide(&l, "plan my trip").await;
    assert_eq!((d.route, d.level), (Route::Agent, Level::K3));
}

#[tokio::test]
async fn k3_malformed_or_invalid_answers_fall_to_agent() {
    let bad = [
        "not json at all",
        "",
        r#"{"route":"workflow","name":"daily_report"}"#, // no confidence
        r#"{"route":"workflow","name":"daily_report","confidence":0.95,"extra":1}"#,
        r#"{"route":"teleport","name":"x","confidence":0.9}"#,
        r#"{"route":"workflow","name":"ghost","confidence":0.99}"#, // not offered
        r#"{"route":"tool","name":"daily_report","confidence":0.99}"#, // wrong kind
        r#"{"route":"workflow","name":"daily_report","confidence":1.5}"#,
        r#"{"route":"workflow","name":"daily_report","params":{"nope":"x"},"confidence":0.99}"#,
        r#"{"route":"workflow","name":"daily_report","params":{"a":[1]},"confidence":0.99}"#,
    ];
    for raw in bad {
        let l = k3_ladder(FakeClassifier::answering(raw));
        let d = decide(&l, "something odd").await;
        assert_eq!(d.route, Route::Agent, "{raw}");
        assert_eq!(d.level, Level::K4, "{raw}");
    }
    let l = k3_ladder(FakeClassifier::answering("{bad"));
    decide(&l, "x y").await;
    assert_eq!(l.metrics().k3_invalid, 1);
}

#[tokio::test]
async fn k3_threshold_and_required_params() {
    // workflow below 0.9 is not routed
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"workflow","name":"daily_report","confidence":0.85}"#,
    ));
    assert_eq!(decide(&l, "report?").await.route, Route::Agent);

    // required param missing
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"workflow","name":"backup_photos","params":{},"confidence":0.99}"#,
    ));
    let d = decide(&l, "backup").await;
    assert_eq!(d.route, Route::Agent);
    assert!(d.reason.contains("folder"), "{}", d.reason);

    // tool hint below k3_hint_threshold
    let l = k3_ladder(FakeClassifier::answering(
        r#"{"route":"tool","name":"take_screenshot","confidence":0.3}"#,
    ));
    assert_eq!(decide(&l, "maybe").await.route, Route::Agent);
}

#[tokio::test]
async fn k3_timeout_and_error_fall_to_agent() {
    let slow = Arc::new(FakeClassifier {
        answer: Ok(r#"{"route":"agent","confidence":1.0}"#.into()),
        delay: Duration::from_secs(5),
        calls: 0.into(),
    });
    let config = LadderConfig {
        k3_timeout: Duration::from_millis(50),
        ..LadderConfig::default()
    };
    let l = base().config(config).classifier(slow).build();
    l.set_candidates(candidates());
    let started = std::time::Instant::now();
    let d = decide(&l, "anything").await;
    assert_eq!((d.route, d.level), (Route::Agent, Level::K4));
    assert!(d.reason.contains("timeout"));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(l.metrics().k3_timeouts, 1);

    let failing = Arc::new(FakeClassifier {
        answer: Err(BackendError::new("HTTP 500")),
        delay: Duration::ZERO,
        calls: 0.into(),
    });
    let l = k3_ladder(failing);
    let d = decide(&l, "anything").await;
    assert_eq!(d.route, Route::Agent);
    assert_eq!(l.metrics().k3_errors, 1);
}

#[tokio::test]
async fn k3_is_one_call_and_skipped_when_k0_k1_k2_decide() {
    let c = FakeClassifier::answering(r#"{"route":"agent","confidence":0.5}"#);
    let l = base()
        .classifier(c.clone())
        .embedder(fake_embedder())
        .build();
    l.set_candidates(candidates());
    l.refresh_embeddings().await.unwrap();
    decide(&l, "/new").await;
    decide(&l, "send me the daily report").await;
    assert_eq!(c.calls.load(Ordering::SeqCst), 0);
    decide(&l, "zzz qqq").await;
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    assert_eq!(l.metrics().k3_calls, 1);
}

#[tokio::test]
async fn empty_and_unconfigured() {
    let l = base().build();
    assert_eq!(decide(&l, "hello").await.route, Route::Agent);
    assert_eq!(decide(&l, "   ").await.reason, "empty request");
    assert_eq!(l.metrics().decided[4], 2);
}

#[test]
fn prompt_is_compact_and_flat() {
    let c = candidates();
    let refs: Vec<&Candidate> = c.iter().collect();
    let p = classifier::build_prompt("line1\nline2 \"quoted\"", &refs);
    assert!(p.contains("backup_photos"));
    assert!(p.contains("[params: folder*]"));
    assert!(p.contains("Request: line1 line2"));
    assert!(p.len() < 1_500, "{}", p.len());
}

// ---------- Gemini clients (wiremock) ----------

#[tokio::test]
async fn gemini_embedder_and_classifier_wire_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/emb-test:batchEmbedContents"))
        .and(header("x-goog-api-key", "k-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embeddings": [{"values": [1.0, 0.0]}, {"values": [0.0, 1.0]}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/models/cls-test:generateContent"))
        .and(header("x-goog-api-key", "k-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"text": "thinking", "thought": true},
                {"text": "{\"route\":\"agent\",\"confidence\":0.9}"}
            ]}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let emb = GeminiEmbedder::new("k-test", "emb-test").with_base_url(server.uri());
    let v = emb
        .embed(&["a".into(), "b".into()], EmbedTask::Document)
        .await
        .unwrap();
    assert_eq!(v, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    assert!(!format!("{emb:?}").contains("k-test"));

    let cls = GeminiClassifier::new("k-test", "cls-test").with_base_url(server.uri());
    let out = cls.classify("p", 64).await.unwrap();
    assert_eq!(out, r#"{"route":"agent","confidence":0.9}"#);
    assert!(!format!("{cls:?}").contains("k-test"));

    // HTTP error -> BackendError without the key
    let bad = GeminiClassifier::new("k-test", "missing").with_base_url(server.uri());
    let err = bad.classify("p", 64).await.unwrap_err();
    assert!(err.to_string().contains("404"));
}
