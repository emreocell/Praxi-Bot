//! Record through the provider boundary, then exercise the real loop offline.
use std::path::PathBuf;

use evals::fake::ScriptedProvider;
use evals::scenario::Scenario;
use evals::tier1::{FAKE_MODEL, capture_scenario, run_scenario};
use evals::transcript::{Chunk, Privacy, ReplayProvider};
use provider_catalog::{ChatProvider, ChatRequest, Message, MessageRole, ModelRef};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/scenarios")
}

#[test]
fn recording_requires_local_owner_opt_in_even_if_ci_says_false() {
    let enabled = |key: &str| (key == "BOT_EVAL_RECORD").then(|| "1".into());
    assert!(evals::transcript::recording_allowed(enabled).is_ok());
    assert!(evals::transcript::recording_allowed(|_| None).is_err());
    let ci = |key: &str| match key {
        "BOT_EVAL_RECORD" => Some("1".into()),
        "CI" => Some("false".into()),
        _ => None,
    };
    assert!(
        evals::transcript::recording_allowed(ci)
            .unwrap_err()
            .to_string()
            .contains("CI")
    );
}

#[tokio::test]
async fn synthetic_recordings_round_trip_and_generate_examples() {
    for (source, name) in [
        ("02-approval-deny.yaml", "recorded-owner-denial"),
        (
            "10-bad-args-and-unknown-tool.yaml",
            "recorded-malformed-recovery",
        ),
        ("inj-mcp-output-run-shell.yaml", "recorded-tainted-executor"),
    ] {
        let mut s =
            Scenario::from_yaml(&std::fs::read_to_string(root().join(source)).unwrap()).unwrap();
        s.name = name.into();
        s.description = format!("Synthetic provider recording: {}", s.description);
        let provider = ScriptedProvider::new(s.script.clone());
        let (result, captured) = capture_scenario(&s, provider, FAKE_MODEL, true)
            .await
            .unwrap();
        assert!(result.record.success, "{:?}", result.record.failures);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recorded.yaml");
        Privacy::default().write(&captured, &path).unwrap();
        let yaml = std::fs::read_to_string(&path).unwrap();
        let replay = Scenario::from_yaml(&yaml).unwrap();
        assert!(replay.transcript.as_ref().unwrap().synthetic);
        let result = run_scenario(&replay).await.unwrap().record;
        assert!(result.success, "{name}: {:?}", result.failures);
        // Explicit maintainer-only fixture regeneration; ordinary tests write only temp files.
        if std::env::var("UPDATE_EVAL_RECORDINGS").as_deref() == Ok("1")
            && std::env::var_os("CI").is_none()
        {
            std::fs::write(root().join(format!("{name}.yaml")), yaml).unwrap();
        }
    }
}

#[tokio::test]
async fn private_canaries_never_reach_the_recorded_file() {
    let canaries = [
        "sk-abcdefghijklmnopqrstuvwxyz123456",
        "owner.canary@example.test",
        "Bearer opaque-credential-canary",
        "C:\\Users\\CanaryOwner\\private\\notes.txt",
        "/home/canary-owner/private/notes.txt",
        "private-project-canary",
        "glpat-abcdefghijklmnopqrstuvwx",
    ];
    let planted = canaries.join(" ; ");
    let s: Scenario = serde_json::from_value(serde_json::json!({
        "name": "privacy", "user": planted, "description": planted,
        "tools": [{"name": "fixture", "effect": "read", "output": planted}],
        "script": [
            {"tool_calls": [{"id": "c1", "tool": "fixture", "args": {"payload": planted}}]},
            {"text": planted},
        ],
    }))
    .unwrap();
    let (_, mut captured) = capture_scenario(
        &s,
        ScriptedProvider::new(s.script.clone()),
        FAKE_MODEL,
        true,
    )
    .await
    .unwrap();
    let t = captured.transcript.as_mut().unwrap();
    let end = t.calls[0].chunks.pop().unwrap();
    t.calls[0].chunks = vec![
        Chunk::Text("sk-abcdefghijklm".into()),
        Chunk::Text("nopqrstuvwxyz123456".into()),
        Chunk::ToolDelta {
            call_id: "c1".into(),
            name: Some("fixture".into()),
            input_delta: "{\"token\":\"sk-abcdefghijklm".into(),
        },
        Chunk::ToolDelta {
            call_id: "c1".into(),
            name: None,
            input_delta: "nopqrstuvwxyz123456\"}".into(),
        },
        Chunk::Tool(provider_catalog::ToolCall::new(
            "c1",
            "fixture",
            serde_json::json!({"nested": {"payload": planted}}).to_string(),
        )),
        end,
    ];
    let mut privacy = Privacy::default();
    privacy.register("private-project-canary");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("safe.yaml");
    privacy.write(&captured, &path).unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();
    for canary in canaries {
        assert!(!disk.contains(canary), "private value reached disk");
    }
    assert!(!disk.contains("sk-abcdefghijklm"));
    assert!(!disk.contains("nopqrstuvwxyz123456"));
    assert!(!disk.contains("CanaryOwner"));
    assert!(!disk.contains("canary-owner"));
    assert!(disk.contains("[REDACTED]"));
    Scenario::from_yaml(&disk).unwrap();
    assert!(
        privacy.write(&captured, &path).is_err(),
        "must not overwrite"
    );
}

#[tokio::test]
async fn divergence_and_truncation_are_actionable_failures() {
    let s = Scenario::from_yaml("name: divergence\nuser: go\nscript:\n  - text: done\n").unwrap();
    let (_, mut captured) = capture_scenario(
        &s,
        ScriptedProvider::new(s.script.clone()),
        FAKE_MODEL,
        true,
    )
    .await
    .unwrap();
    let t = captured.transcript.as_mut().unwrap();
    let p = ReplayProvider::new(t.clone()).unwrap();
    let wrong = ChatRequest::new(
        ModelRef(FAKE_MODEL.into()),
        vec![Message::tool_result("unexpected", "different result")],
    );
    let err = p.stream(wrong).await.err().unwrap().to_string();
    assert!(err.contains("call 0") && err.contains("diverged"), "{err}");
    let p = ReplayProvider::new(t.clone()).unwrap();
    assert!(p.failures()[0].contains("consumed 0 of 1"));
    // An additional invocation after the recording ends fails independently.
    let request = ChatRequest::new(
        ModelRef(FAKE_MODEL.into()),
        vec![Message::text(MessageRole::User, "go")],
    );
    let p = ReplayProvider::new(t.clone()).unwrap();
    let _ = p.stream(request.clone()).await;
    assert!(
        p.stream(request)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("exhausted")
    );
    t.calls[0].chunks.pop();
    assert!(t.validate().unwrap_err().to_string().contains("truncated"));
    assert!(ReplayProvider::new(t.clone()).is_err());
}
