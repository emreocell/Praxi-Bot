use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError, ToolOutput};

use super::super::fake::FakeBrowser;
use super::super::{DesktopControlGate, tools};
use super::{MAX_STEPS, can_egress};
use crate::control::ControlRequest;

fn act_tool(fake: &Arc<FakeBrowser>, gate: Option<Arc<dyn DesktopControlGate>>) -> Arc<dyn Tool> {
    let dir = tempfile::tempdir().unwrap();
    tools(fake.clone(), dir.path(), gate)
        .into_iter()
        .find(|t| t.spec().name == "browser_act")
        .unwrap()
}

fn text(o: &ToolOutput) -> String {
    match &o.content[0] {
        ContentPart::Text { text } => text.clone(),
        _ => panic!(),
    }
}

fn calls(fake: &FakeBrowser) -> Vec<String> {
    fake.calls.lock().clone()
}

fn three() -> Value {
    json!({"steps":[
        {"action":"fill","ref":"e1","text":"typed-private-text"},
        {"action":"select","ref":"e2","value":"blue"},
        {"action":"click","ref":"e3"}
    ]})
}

#[tokio::test]
async fn steps_run_in_order_and_typed_text_is_not_echoed_in_the_report() {
    let fake = Arc::new(FakeBrowser::default());
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), three())
        .await
        .unwrap();
    assert_eq!(
        calls(&fake),
        ["fill:e1:typed-private-text", "select:e2:blue", "click:e3"]
    );
    let t = text(&out);
    assert!(t.contains("browser_act: done 3/3"), "{t}");
    assert!(t.contains("1. fill e1 (18 characters) ok"), "{t}");
    assert!(!t.contains("typed-private-text"), "typed text leaked: {t}");
}

#[tokio::test]
async fn a_changed_element_stops_the_batch_before_it_is_clicked() {
    let fake = Arc::new(FakeBrowser::default());
    let f2 = fake.clone();
    *fake.hook.lock() = Some(Arc::new(move |s: &str| {
        if s.starts_with("fill:e1") {
            // The page re-rendered: e2 is now something else.
            f2.identities
                .lock()
                .insert("e2".into(), ("button".into(), "Delete account".into()));
        }
    }));
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), three())
        .await
        .unwrap();
    let t = text(&out);
    assert_eq!(calls(&fake), ["fill:e1:typed-private-text"], "{t}");
    assert!(t.contains("stopped, done 1/3"), "{t}");
    assert!(t.contains("step 2 not run"), "{t}");
    assert!(
        t.contains("not run:\n2. select e2") && t.contains("3. click e3"),
        "{t}"
    );
}

#[tokio::test]
async fn navigation_stops_the_batch_after_the_step_that_caused_it() {
    let fake = Arc::new(FakeBrowser::default());
    *fake.url.lock() = Some("https://a.test/".into());
    let f2 = fake.clone();
    *fake.hook.lock() = Some(Arc::new(move |s: &str| {
        if s.starts_with("select:e2") {
            *f2.url.lock() = Some("https://a.test/next".into());
        }
    }));
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), three())
        .await
        .unwrap();
    let t = text(&out);
    assert_eq!(calls(&fake).len(), 2, "{t}");
    assert!(
        t.contains("stopped, done 2/3") && t.contains("navigated"),
        "{t}"
    );
}

#[tokio::test]
async fn a_failing_step_in_the_middle_stops_and_names_the_step() {
    let fake = Arc::new(FakeBrowser::default());
    // The fake fails any call that mentions e404; a press has no ref to fingerprint.
    let input = json!({"steps":[
        {"action":"click","ref":"e1"},
        {"action":"press","key":"e404"},
        {"action":"click","ref":"e3"}
    ]});
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), input)
        .await
        .unwrap();
    let t = text(&out);
    assert!(
        t.contains("stopped, done 1/3: step 2 (press) failed"),
        "{t}"
    );
    assert_eq!(calls(&fake).len(), 2, "step 3 must not run: {t}");
}

#[tokio::test]
async fn an_unverifiable_ref_refuses_the_whole_batch_before_step_one() {
    let fake = Arc::new(FakeBrowser::default());
    let input = json!({"steps":[
        {"action":"click","ref":"e1"},
        {"action":"click","ref":"e404"}
    ]});
    let err = act_tool(&fake, None)
        .call(&ToolContext::default(), input)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nothing was done"), "{err}");
    assert_eq!(calls(&fake), Vec::<String>::new());
}

#[tokio::test]
async fn the_batch_is_bounded_and_refuses_what_is_not_a_page_step() {
    let fake = Arc::new(FakeBrowser::default());
    let t = act_tool(&fake, None);
    let many: Vec<Value> = (0..=MAX_STEPS)
        .map(|i| json!({"action":"click","ref":format!("e{i}")}))
        .collect();
    for bad in [
        json!({"steps": many}),
        json!({"steps": []}),
        json!({"steps":[{"action":"open","url":"https://x.test"}]}),
        json!({"steps":[{"action":"eval","js":"1"}]}),
        json!({"steps":[{"action":"fill","ref":"e1","secret":"login/x"}]}),
        json!({"steps":[{"action":"click","ref":"e1","prompt_text":"x"}]}),
        json!({"steps":[{"action":"wait","idle":true,"timeout_secs":99}]}),
        json!({"steps":[{"action":"wait"}]}),
        json!({"steps":[{"action":"click","ref":"e1"}],"extra":1}),
    ] {
        let err = t
            .call(&ToolContext::default(), bad.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
    }
    assert_eq!(calls(&fake), Vec::<String>::new());
}

/// Lets the first action through, refuses every later one (a human took over).
struct TakeoverAfterOne(std::sync::atomic::AtomicUsize);

#[async_trait]
impl DesktopControlGate for TakeoverAfterOne {
    async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            Err("the user has taken control".into())
        }
    }
}

#[tokio::test]
async fn every_step_goes_through_the_control_gate_like_its_single_tool() {
    let fake = Arc::new(FakeBrowser {
        on_desktop: true,
        ..FakeBrowser::default()
    });
    let gate = Arc::new(TakeoverAfterOne(std::sync::atomic::AtomicUsize::new(0)));
    let out = act_tool(&fake, Some(gate.clone()))
        .call(&ToolContext::default(), three())
        .await
        .unwrap();
    let t = text(&out);
    assert_eq!(calls(&fake), ["fill:e1:typed-private-text"], "{t}");
    assert!(t.contains("done 1/3") && t.contains("taken control"), "{t}");
    assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_approval_card_lists_every_step_and_is_classified_like_the_single_actions() {
    let fake = Arc::new(FakeBrowser::default());
    let tool = act_tool(&fake, None);
    let d = tool
        .approval_details(&ToolContext::default(), &three())
        .await
        .unwrap()
        .expect("a batch always has card details (so it is never grantable for good)");
    assert_eq!(d["kind"], "browser_steps");
    let steps = d["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["text"], "typed-private-text");
    assert_eq!(steps[1]["value"], "blue");
    assert_eq!(steps[2]["target"], "button \"name of e3\"");
    assert_eq!(tool.spec().effect, tool_registry::Effect::Write);
    assert_eq!(
        tool.spec().category,
        tool_registry::PermissionCategory::Browser
    );
    assert!(crate::classify("browser_act").unwrap().egress);
    // An invalid batch has no card at all: it is refused before anything is asked.
    assert!(
        tool.approval_details(&ToolContext::default(), &json!({"steps": []}))
            .await
            .is_err()
    );
}

#[test]
fn only_batches_that_type_select_or_press_can_carry_bytes() {
    assert!(can_egress(&three()));
    assert!(can_egress(
        &json!({"steps":[{"action":"press","key":"Enter"}]})
    ));
    assert!(can_egress(&json!("not a batch")));
    assert!(!can_egress(&json!({"steps":[
        {"action":"click","ref":"e1"},
        {"action":"scroll","dy":300},
        {"action":"wait","idle":true}
    ]})));
}

#[test]
fn the_summary_names_every_step_without_the_typed_text_and_oversize_input_is_refused() {
    let s = super::summary(&three()).unwrap();
    assert!(
        s.starts_with("3 steps: 1. fill e1 (18 characters); 2. select e2"),
        "{s}"
    );
    assert!(
        s.contains("3. click e3") && !s.contains("typed-private-text"),
        "{s}"
    );
    assert!(super::summary(&json!({"steps": []})).is_none());
    let long = json!({"steps":[{"action":"fill","ref":"e1","text":"x".repeat(3000)}]});
    assert!(matches!(
        super::parse_steps(&long),
        Err(ToolError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn an_overlay_on_top_of_the_target_stops_the_batch_and_nothing_is_clicked() {
    let fake = Arc::new(FakeBrowser::default());
    // Step 1 opens a modal whose "Pay" button now covers e2 ("No thanks").
    let f2 = fake.clone();
    *fake.hook.lock() = Some(Arc::new(move |s: &str| {
        if s == "click:e1" {
            f2.covered.lock().insert("e2".into());
        }
    }));
    let input = json!({"steps": [
        {"action": "click", "ref": "e1"},
        {"action": "click", "ref": "e2"},
        {"action": "click", "ref": "e3"},
    ]});
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), input)
        .await
        .unwrap();
    let t = text(&out);
    assert_eq!(calls(&fake), ["click:e1"], "{t}");
    assert!(t.contains("stopped, done 1/3"), "{t}");
    assert!(t.contains("step 2 (click) failed"), "{t}");
    assert!(t.contains("3. click e3"), "{t}");
}

#[tokio::test]
async fn enter_only_goes_to_the_element_the_previous_step_worked_on() {
    let input = json!({"steps": [
        {"action": "fill", "ref": "e1", "text": "me@example.com"},
        {"action": "press", "key": "Enter"},
    ]});
    // Focus still on the filled field: pressed.
    let fake = Arc::new(FakeBrowser::default());
    *fake.focus.lock() = Some("e1".into());
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), input.clone())
        .await
        .unwrap();
    assert!(text(&out).contains("done 2/2"), "{}", text(&out));
    // A page script moved focus to another control: not pressed, step 2 named.
    let fake = Arc::new(FakeBrowser::default());
    *fake.focus.lock() = Some("e9".into());
    let out = act_tool(&fake, None)
        .call(&ToolContext::default(), input.clone())
        .await
        .unwrap();
    let t = text(&out);
    assert_eq!(calls(&fake), ["fill:e1:me@example.com"], "{t}");
    assert!(t.contains("step 2 (press) failed"), "{t}");
    // Enter as the first step has no earlier node to compare with: not pressed.
    let fake = Arc::new(FakeBrowser::default());
    *fake.focus.lock() = Some("e1".into());
    let first = json!({"steps": [{"action": "press", "key": "Enter"}]});
    let _ = act_tool(&fake, None)
        .call(&ToolContext::default(), first)
        .await;
    assert!(calls(&fake).is_empty(), "{:?}", calls(&fake));
}
