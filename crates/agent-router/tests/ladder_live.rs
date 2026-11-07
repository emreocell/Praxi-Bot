//! Live Gemini check of the ladder (EN + TR). Budget: 1 embedding batch + 10
//! query embeddings + at most 10 classifier calls = at most 21 requests.
//!
//! Run manually:
//! `GEMINI_API_KEY=... cargo test -p agent-router --test ladder_live -- --ignored --nocapture`
//! (optional `GEMINI_CLASSIFIER_MODEL`, `GEMINI_EMBED_MODEL`). The key is only
//! read from the environment and never printed.

use std::sync::Arc;
use std::time::Duration;

use agent_router::ladder::{GeminiClassifier, GeminiEmbedder};
use agent_router::{Candidate, CandidateKind, Ladder, Route, RouteContext};

fn corpus() -> Vec<Candidate> {
    vec![
        Candidate::new(
            CandidateKind::Workflow,
            "daily_report",
            "Send the daily sales report by email",
        ),
        Candidate::new(
            CandidateKind::Workflow,
            "backup_photos",
            "Copy photos from a folder to the backup drive",
        )
        .with_param("folder", true),
        Candidate::new(
            CandidateKind::Tool,
            "take_screenshot",
            "Capture the screen to an image",
        ),
        Candidate::new(
            CandidateKind::Tool,
            "read_file",
            "Read a text file from disk",
        ),
        Candidate::new(
            CandidateKind::Tool,
            "http_fetch",
            "Fetch a web page over HTTP",
        ),
        Candidate::new(
            CandidateKind::Skill,
            "instagram_post",
            "Publish a photo post to Instagram",
        ),
    ]
}

/// (utterance, expected kind, expected name)
const CASES: [(&str, &str, &str); 10] = [
    ("send me the daily sales report", "workflow", "daily_report"),
    ("bugünün satış raporunu gönder", "workflow", "daily_report"),
    ("take a screenshot of my screen", "tool", "take_screenshot"),
    ("ekran görüntüsü al", "tool", "take_screenshot"),
    (
        "open notes.txt and show me what is inside",
        "tool",
        "read_file",
    ),
    ("dosyanın içeriğini oku", "tool", "read_file"),
    ("post this photo on instagram", "skill", "instagram_post"),
    (
        "fotoğrafları D:\\Resimler klasöründen yedekle",
        "workflow",
        "backup_photos",
    ),
    (
        "plan a 3-day trip to Rome and book the cheapest flights",
        "agent",
        "",
    ),
    ("yarınki toplantı için bir sunum hazırla", "agent", ""),
];

fn matches(route: &Route, kind: &str, name: &str) -> bool {
    match (route, kind) {
        (Route::Workflow { name: n, .. }, "workflow") => n == name,
        (Route::ToolHint { tools }, "tool") => tools.iter().any(|t| t == name),
        (Route::SkillHint { skills }, "skill") => skills.iter().any(|s| s == name),
        (Route::Agent, "agent") => true,
        _ => false,
    }
}

#[tokio::test]
#[ignore = "calls the live Gemini API (<= 21 requests)"]
async fn live_ladder_en_tr() {
    let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let cls_model = std::env::var("GEMINI_CLASSIFIER_MODEL")
        .unwrap_or_else(|_| "gemini-2.5-flash-lite".to_owned());
    let emb_model =
        std::env::var("GEMINI_EMBED_MODEL").unwrap_or_else(|_| "gemini-embedding-001".to_owned());

    let ladder = Ladder::builder()
        .embedder(Arc::new(GeminiEmbedder::new(key.clone(), emb_model)))
        .classifier(Arc::new(GeminiClassifier::new(key, cls_model)))
        .build();
    ladder.set_candidates(corpus());
    ladder.refresh_embeddings().await.expect("embed corpus");

    let (mut correct, mut total_ms) = (0_u32, 0_u64);
    for (i, (text, kind, name)) in CASES.iter().enumerate() {
        let d = ladder.decide(&RouteContext::new(*text)).await;
        let ok = matches(&d.route, kind, name);
        correct += u32::from(ok);
        total_ms += d.latency_ms;
        println!(
            "[{}] {} | {text:?} -> {:?} {:?} conf={:.2} {}ms | {}",
            i + 1,
            if ok { "OK  " } else { "MISS" },
            d.level,
            d.route,
            d.confidence,
            d.latency_ms,
            d.reason
        );
        // stay under the free tier's 15 RPM for the classifier
        tokio::time::sleep(Duration::from_millis(4500)).await;
    }
    let m = ladder.metrics();
    println!(
        "accuracy {correct}/10, mean latency {} ms, requests ~{} (1 corpus embed + 10 query embeds + {} classifier), metrics {m:?}",
        total_ms / 10,
        1 + 10 + m.k3_calls,
        m.k3_calls
    );
}
