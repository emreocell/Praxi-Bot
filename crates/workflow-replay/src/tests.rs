//! End-to-end scenarios over fake backends.

use std::sync::Arc;

use desktop_api::{Action, ClickMethod, Target};
use parking_lot::Mutex;
use protocol::{Event, RunId, UserId};
use serde_json::{Value, json};
use storage::RunStep;
use tool_registry::{ToolContext, ToolError, ToolRegistry};
use uuid::Uuid;

use crate::format::{
    AgentStep, BrowserAction, BrowserActionKind, BrowserLocator, DesktopAction, DesktopActionKind,
    DesktopLocator, Launch, Param, ParamType, Step, SuccessCheck, ToolStep, Workflow,
};
use crate::locator::fingerprint_hex;
use crate::recorder::{RecordOptions, Recorder};
use crate::replay::{EventSink, Handles, HandoverReason, ReplayOptions, Replayer};
use crate::store::WorkflowStore;
use crate::testkit::{CaptureTool, FakeBrowser, FakeDesktop, RecordingGate, node, window};

const NOTEPAD_TREE: &str = "# window \"Untitled - Notepad\" pid=101 gen=1 quality=Good nodes=2\n[0] button \"Close\" (790,10)\n[1] edit \"Text editor\" (400,300) {focused}\n";

fn run_step(idx: u32, name: &str, input: Value, output: Option<Value>) -> RunStep {
    RunStep {
        id: Uuid::now_v7(),
        run_id: RunId::new(),
        user_id: UserId::new(),
        idx,
        name: name.into(),
        input,
        idempotent: false,
        retry: None,
        status: "succeeded".into(),
        attempt: 1,
        output,
        error: None,
        started_at_ms: 0,
        finished_at_ms: Some(0),
    }
}

fn tool_call(idx: u32, id: &str, name: &str, args: &Value, result: &str) -> RunStep {
    run_step(
        idx,
        "tools",
        json!({"calls": [{"id": id, "name": name, "arguments": args.to_string(), "repeat": 1}]}),
        Some(
            json!({"results": [{"id": id, "call_uuid": Uuid::now_v7(), "name": name,
            "is_error": false, "text": result, "spill_path": null}]}),
        ),
    )
}

/// The scripted agent run: "open notepad and type Hello World".
fn scripted_notepad_run() -> Vec<RunStep> {
    vec![
        run_step(
            0,
            "user_message",
            json!({"text": "open notepad and type Hello World"}),
            None,
        ),
        tool_call(
            1,
            "a",
            "desktop_launch",
            &json!({"executable": "notepad.exe"}),
            "launched pid=101",
        ),
        tool_call(2, "b", "desktop_get_state", &json!({}), NOTEPAD_TREE),
        tool_call(
            3,
            "c",
            "desktop_type",
            &json!({"index": 1, "text": "Hello World"}),
            NOTEPAD_TREE,
        ),
    ]
}

fn notepad_desktop() -> Arc<FakeDesktop> {
    let d = FakeDesktop::new(
        vec![],
        vec![
            node(0, "button", "Close", 790.0, 10.0),
            node(1, "edit", "Text editor", 400.0, 300.0),
        ],
    );
    *d.on_launch.lock() = Some(window(1, "Untitled - Notepad"));
    d
}

fn handles(desktop: Option<Arc<FakeDesktop>>, browser: Option<Arc<FakeBrowser>>) -> Handles {
    Handles {
        desktop: desktop.map(|d| d as Arc<dyn desktop_api::DesktopBackend>),
        browser: browser.map(|b| b as Arc<dyn builtin_tools::BrowserDriver>),
        tools: ToolRegistry::new(),
    }
}

fn collector() -> (EventSink, Arc<Mutex<Vec<Event>>>) {
    let store = Arc::new(Mutex::new(Vec::new()));
    let s = store.clone();
    (Arc::new(move |e| s.lock().push(e)), store)
}

fn params(v: &Value) -> serde_json::Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn desk_loc(role: &str, name: &str) -> DesktopLocator {
    DesktopLocator {
        role: role.into(),
        name: name.into(),
        fingerprint: fingerprint_hex(role, name),
        automation_id: None,
        class_name: None,
        nth: None,
        position: None,
    }
}

fn click(window: &str, loc: DesktopLocator) -> Step {
    Step::DesktopAction(DesktopAction {
        app: None,
        window_title_pattern: Some(window.into()),
        target: Some(loc),
        action: DesktopActionKind::Click,
        value: None,
        method: None,
    })
}

fn screen_wf(steps: Vec<Step>) -> Workflow {
    let mut w = Workflow::new("flow");
    w.steps = steps;
    w
}

#[tokio::test]
async fn record_from_scripted_run_then_replay_with_zero_model_calls() {
    let rec = Recorder::new()
        .from_steps(
            &scripted_notepad_run(),
            &RecordOptions {
                name: Some("notepad_note".into()),
                description: None,
            },
        )
        .unwrap();
    assert!(rec.dropped.is_empty(), "{:?}", rec.dropped);
    // Round-trip through the store: what is replayed is what was reviewed and saved.
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    store.save(&rec.workflow).unwrap();
    let wf = store.get("notepad_note").unwrap();
    assert_eq!(wf.params.len(), 1);

    let desktop = notepad_desktop();
    let gate = RecordingGate::default();
    let (sink, events) = collector();
    let mut opts = ReplayOptions::instant();
    opts.events = Some(sink);
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({"text": "Goodbye"})), &gate)
        .await
        .unwrap();

    assert!(report.is_completed(), "{report:?}");
    assert_eq!(report.llm_calls, 0);
    assert_eq!(report.steps_replayed, 2);
    assert_eq!(desktop.launched.lock().as_slice(), ["notepad.exe"]);
    let actions = desktop.actions.lock();
    assert!(matches!(
        &actions[0],
        Action::Click {
            target: Target::ByIndex { index: 1, .. },
            method: ClickMethod::Global
        }
    ));
    assert!(matches!(&actions[1], Action::TypeText { text } if text == "Goodbye"));
    // Same gate as the agent loop: launch and type were both authorized.
    assert_eq!(gate.tools(), ["desktop_launch", "desktop_type"]);
    let ev = events.lock();
    let started = ev
        .iter()
        .filter(|e| matches!(e, Event::WorkflowStepStarted(_)))
        .count();
    let done = ev
        .iter()
        .filter(|e| matches!(e, Event::WorkflowStepCompleted(_)))
        .count();
    assert_eq!((started, done), (2, 2));
}

#[tokio::test]
async fn renamed_button_hands_over_at_the_right_step() {
    let wf = screen_wf(vec![
        Step::Launch(Launch {
            executable: "app.exe".into(),
            args: vec![],
        }),
        click("*App", desk_loc("button", "Save")),
        click("*App", desk_loc("button", "Submit")),
        click("*App", desk_loc("button", "Done")),
    ]);
    // "Submit" was renamed to "Send".
    let desktop = FakeDesktop::new(
        vec![window(1, "Doc - App")],
        vec![
            node(0, "button", "Save", 10.0, 10.0),
            node(1, "button", "Send", 50.0, 10.0),
            node(2, "button", "Done", 90.0, 10.0),
        ],
    );
    let (sink, events) = collector();
    let mut opts = ReplayOptions::instant();
    opts.events = Some(sink);
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();

    let h = report.handover().expect("must hand over");
    assert_eq!(h.step_index, 2);
    assert!(
        matches!(h.reason, HandoverReason::Drift { .. }),
        "{:?}",
        h.reason
    );
    assert_eq!(h.partial_state.completed, 2);
    assert_eq!(h.partial_state.remaining.len(), 2);
    assert!(h.partial_state.remaining[0].contains("Submit"));
    // Only the Save click happened; nothing after the drift was touched.
    assert_eq!(desktop.actions.lock().len(), 1);
    assert!(
        events
            .lock()
            .iter()
            .any(|e| matches!(e, Event::WorkflowDrift(d) if d.step_index == 2))
    );
}

#[tokio::test]
async fn ambiguous_locator_is_a_drift_never_a_first_pick() {
    let wf = screen_wf(vec![click("*App", desk_loc("button", "OK"))]);
    let desktop = FakeDesktop::new(
        vec![window(1, "Doc - App")],
        vec![
            node(0, "button", "OK", 10.0, 10.0),
            node(1, "button", "OK", 50.0, 10.0),
        ],
    );
    let report = Replayer::new(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(&wf, &params(&json!({})), &RecordingGate::default())
    .await
    .unwrap();
    let h = report.handover().unwrap();
    assert!(
        matches!(h.reason, HandoverReason::Ambiguous { .. }),
        "{:?}",
        h.reason
    );
    assert_eq!(h.step_index, 0);
    assert!(desktop.actions.lock().is_empty());
}

#[tokio::test]
async fn ambiguous_window_is_a_drift_too() {
    let wf = screen_wf(vec![click("*App", desk_loc("button", "OK"))]);
    let desktop = FakeDesktop::new(
        vec![window(1, "A - App"), window(2, "B - App")],
        vec![node(0, "button", "OK", 10.0, 10.0)],
    );
    let report = Replayer::new(handles(Some(desktop), None), ReplayOptions::instant())
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(matches!(
        report.handover().unwrap().reason,
        HandoverReason::Ambiguous { .. }
    ));
}

#[tokio::test]
async fn params_are_substituted_typed_and_validated() {
    let page = "# Shop (https://shop.test/cart)\n  [e1] textbox \"Search\"\n  [e2] button \"Go\"\n";
    let browser = FakeBrowser::new(page);
    let capture = CaptureTool::new("shop_log");
    let tools = ToolRegistry::new();
    assert!(tools.register(capture.clone()));

    let mut wf = Workflow::new("shop");
    wf.params = vec![
        Param {
            name: "query".into(),
            ty: ParamType::String,
            default: Some("red shoes".into()),
            description: String::new(),
        },
        Param {
            name: "qty".into(),
            ty: ParamType::Number,
            default: Some("1".into()),
            description: String::new(),
        },
    ];
    let bloc = |role: &str, name: &str| BrowserLocator {
        role: role.into(),
        name: name.into(),
        fingerprint: fingerprint_hex(role, name),
        aria_label: None,
        css: None,
        nth: None,
    };
    wf.steps = vec![
        Step::BrowserAction(BrowserAction {
            url_pattern: Some("https://shop.test/*".into()),
            target: Some(bloc("textbox", "Search")),
            action: BrowserActionKind::Fill,
            value: Some("{query} x{qty}".into()),
        }),
        Step::BrowserAction(BrowserAction {
            url_pattern: None,
            target: Some(bloc("button", "Go")),
            action: BrowserActionKind::Click,
            value: None,
        }),
        Step::Tool(ToolStep {
            tool: "shop_log".into(),
            input: json!({"q": "{query}", "qty": "{qty}", "lit": "{{braces}}"}),
        }),
    ];
    let h = Handles {
        desktop: None,
        browser: Some(browser.clone()),
        tools,
    };
    let rp = Replayer::new(h, ReplayOptions::instant());
    let gate = RecordingGate::default();
    let report = rp
        .replay(&wf, &params(&json!({"query": "blue hat", "qty": 3})), &gate)
        .await
        .unwrap();
    assert!(report.is_completed(), "{report:?}");
    assert_eq!(
        browser.calls.lock().as_slice(),
        ["fill:e1:blue hat x3", "click:e2"]
    );
    assert_eq!(
        capture.inputs.lock()[0],
        json!({"q": "blue hat", "qty": 3, "lit": "{braces}"})
    );
    assert_eq!(gate.tools(), ["browser_fill", "browser_click", "shop_log"]);

    // Defaults apply; bad types / unknown names are rejected before anything runs.
    let ok = rp.replay(&wf, &params(&json!({})), &gate).await.unwrap();
    assert!(ok.is_completed());
    assert_eq!(browser.calls.lock()[2], "fill:e1:red shoes x1");
    assert!(
        rp.replay(&wf, &params(&json!({"qty": "many"})), &gate)
            .await
            .is_err()
    );
    assert!(
        rp.replay(&wf, &params(&json!({"nope": 1})), &gate)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn permission_deny_stops_the_replay() {
    let wf = screen_wf(vec![
        click("*App", desk_loc("button", "Save")),
        click("*App", desk_loc("button", "Done")),
    ]);
    let desktop = FakeDesktop::new(
        vec![window(1, "Doc - App")],
        vec![
            node(0, "button", "Save", 10.0, 10.0),
            node(1, "button", "Done", 50.0, 10.0),
        ],
    );
    let gate = RecordingGate::denying("desktop_click");
    let report = Replayer::new(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(&wf, &params(&json!({})), gate.as_ref())
    .await
    .unwrap();
    let h = report.handover().unwrap();
    assert!(matches!(h.reason, HandoverReason::PermissionDenied { .. }));
    assert_eq!(h.step_index, 0);
    assert!(
        desktop.actions.lock().is_empty(),
        "denied step must not run"
    );
    assert_eq!(gate.tools().len(), 1, "replay stopped at the first denial");
}

#[tokio::test]
async fn agent_step_and_failed_success_check_hand_over() {
    let browser = FakeBrowser::new("# P (https://a.test/)\n  [e1] button \"Go\"\n");
    let mut wf = Workflow::new("w");
    wf.params.push(Param {
        name: "who".into(),
        ty: ParamType::String,
        default: Some("Ada".into()),
        description: String::new(),
    });
    wf.steps = vec![Step::Agent(AgentStep {
        instruction: "Greet {who}".into(),
    })];
    let rp = Replayer::new(
        handles(None, Some(browser.clone())),
        ReplayOptions::instant(),
    );
    let r = rp
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    let h = r.handover().unwrap();
    assert_eq!(h.step_index, 0);
    assert!(
        matches!(&h.reason, HandoverReason::AgentStep { instruction } if instruction == "Greet Ada")
    );

    wf.steps.clear();
    wf.success_check = Some(SuccessCheck::BrowserText {
        contains: "Welcome {who}".into(),
    });
    let r = rp
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(matches!(
        r.handover().unwrap().reason,
        HandoverReason::SuccessCheckFailed { .. }
    ));
    *browser.page.lock() = "# P (https://a.test/)\n  heading \"Welcome Ada\"\n".into();
    assert!(
        rp.replay(&wf, &params(&json!({})), &RecordingGate::default())
            .await
            .unwrap()
            .is_completed()
    );
}

#[tokio::test]
async fn browser_css_fallback_needs_a_unique_match() {
    let browser = FakeBrowser::new("# P (https://a.test/)\n  [e1] button \"Other\"\n");
    let mut wf = Workflow::new("w");
    wf.steps = vec![Step::BrowserAction(BrowserAction {
        url_pattern: None,
        target: Some(BrowserLocator {
            role: "button".into(),
            name: "Gone".into(),
            fingerprint: fingerprint_hex("button", "Gone"),
            aria_label: None,
            css: Some("#go".into()),
            nth: None,
        }),
        action: BrowserActionKind::Click,
        value: None,
    })];
    let rp = Replayer::new(
        handles(None, Some(browser.clone())),
        ReplayOptions::instant(),
    );
    let gate = RecordingGate::default();
    let r = rp.replay(&wf, &params(&json!({})), &gate).await.unwrap();
    assert!(r.is_completed());
    assert_eq!(r.steps[0].resolved_via.as_deref(), Some("css"));
    assert_eq!(gate.tools(), ["browser_eval"]);

    *browser.eval_result.lock() = json!(2);
    let r = rp.replay(&wf, &params(&json!({})), &gate).await.unwrap();
    assert!(matches!(
        r.handover().unwrap().reason,
        HandoverReason::Ambiguous { .. }
    ));
    *browser.eval_result.lock() = json!(0);
    let r = rp.replay(&wf, &params(&json!({})), &gate).await.unwrap();
    assert!(matches!(
        r.handover().unwrap().reason,
        HandoverReason::Drift { .. }
    ));
}

#[tokio::test]
async fn browser_url_mismatch_is_a_drift() {
    let browser = FakeBrowser::new("# P (https://evil.test/)\n  [e1] button \"Go\"\n");
    let mut wf = Workflow::new("w");
    wf.steps = vec![Step::BrowserAction(BrowserAction {
        url_pattern: Some("https://shop.test/*".into()),
        target: Some(BrowserLocator {
            role: "button".into(),
            name: "Go".into(),
            fingerprint: fingerprint_hex("button", "Go"),
            aria_label: None,
            css: None,
            nth: None,
        }),
        action: BrowserActionKind::Click,
        value: None,
    })];
    let r = Replayer::new(
        handles(None, Some(browser.clone())),
        ReplayOptions::instant(),
    )
    .replay(&wf, &params(&json!({})), &RecordingGate::default())
    .await
    .unwrap();
    assert!(r.handover().is_some());
    assert!(
        browser.calls.lock().is_empty(),
        "must not click on the wrong site"
    );
}

#[tokio::test]
async fn workflow_run_tool_replays_and_reports_handover_and_denial() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    let mut wf = screen_wf(vec![
        click("*App", desk_loc("button", "Save")),
        click("*App", desk_loc("button", "Gone")),
    ]);
    wf.name = "two_clicks".into();
    wf.description = "Save then Gone".into();
    store.save(&wf).unwrap();
    let desktop = FakeDesktop::new(
        vec![window(1, "Doc - App")],
        vec![node(0, "button", "Save", 10.0, 10.0)],
    );
    let gate = RecordingGate::default();
    let tools = crate::tools(
        store.clone(),
        handles(Some(desktop), None),
        Arc::new(gate),
        ReplayOptions::instant(),
    );
    let run = tools
        .iter()
        .find(|t| t.spec().name == "workflow_run")
        .unwrap();
    let list = tools
        .iter()
        .find(|t| t.spec().name == "workflow_list")
        .unwrap();
    assert_eq!(run.spec().effect, tool_registry::Effect::Write);

    let ctx = ToolContext::default();
    let out = run.call(&ctx, json!({"name": "two_clicks"})).await.unwrap();
    let tool_registry::ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(text.contains("HANDOVER at step 1"), "{text}");
    assert!(text.contains("Gone"), "{text}");
    assert!(run.call(&ctx, json!({"name": "missing"})).await.is_err());

    let out = list.call(&ctx, json!({})).await.unwrap();
    let tool_registry::ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(text.contains("two_clicks"));

    // A denied step surfaces as a permission error, not a quiet handover.
    let denied = crate::tools(
        store,
        handles(
            Some(FakeDesktop::new(
                vec![window(1, "Doc - App")],
                vec![node(0, "button", "Save", 10.0, 10.0)],
            )),
            None,
        ),
        RecordingGate::denying("desktop_click"),
        ReplayOptions::instant(),
    );
    let run = denied
        .iter()
        .find(|t| t.spec().name == "workflow_run")
        .unwrap();
    let err = run
        .call(&ctx, json!({"name": "two_clicks"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
}

#[tokio::test]
async fn shell_step_quotes_parameter_values() {
    let capture = CaptureTool::new("shell_run");
    let tools = ToolRegistry::new();
    assert!(tools.register(capture.clone()));
    let mut wf = Workflow::new("echoer");
    wf.params = vec![Param {
        name: "input".into(),
        ty: ParamType::String,
        default: None,
        description: String::new(),
    }];
    wf.steps = vec![Step::Shell(crate::format::Shell {
        command: "echo {input}".into(),
        shell: Some("sh".into()),
        cwd: None,
    })];
    let h = Handles {
        desktop: None,
        browser: None,
        tools,
    };
    let rp = Replayer::new(h, ReplayOptions::instant());
    let report = rp
        .replay(
            &wf,
            &params(&json!({"input": "x; curl evil"})),
            &RecordingGate::default(),
        )
        .await
        .unwrap();
    assert!(report.is_completed(), "{report:?}");
    assert_eq!(
        capture.inputs.lock()[0]["command"],
        json!("echo 'x; curl evil'")
    );
}

#[tokio::test]
async fn gate_sees_the_resolved_tool_name_not_the_alias() {
    let capture = CaptureTool::new("mcp_srv.delete_repo");
    let tools = ToolRegistry::new();
    assert!(tools.register(capture));
    let mut wf = Workflow::new("del");
    wf.steps = vec![Step::Tool(ToolStep {
        tool: "mcp_srv__delete_repo".into(),
        input: json!({}),
    })];
    let h = Handles {
        desktop: None,
        browser: None,
        tools,
    };
    let gate = RecordingGate::default();
    let report = Replayer::new(h, ReplayOptions::instant())
        .replay(&wf, &params(&json!({})), &gate)
        .await
        .unwrap();
    assert!(report.is_completed(), "{report:?}");
    assert_eq!(gate.tools(), ["mcp_srv.delete_repo"]);
}

#[tokio::test]
async fn workflow_cannot_call_itself_through_workflow_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    let mut wf = Workflow::new("loopy");
    wf.description = "calls itself".into();
    wf.steps = vec![Step::Tool(ToolStep {
        tool: "workflow_run".into(),
        input: json!({"name": "loopy"}),
    })];
    store.save(&wf).unwrap();
    let registry = ToolRegistry::new();
    let h = Handles {
        desktop: None,
        browser: None,
        tools: registry.clone(),
    };
    let gate: Arc<dyn builtin_tools::ToolGate> = Arc::new(RecordingGate::default());
    for t in crate::tools(
        store.clone(),
        h.clone(),
        gate.clone(),
        ReplayOptions::instant(),
    ) {
        assert!(registry.register(t));
    }
    let run = registry.get("workflow_run").unwrap();
    let out = run
        .call(&ToolContext::default(), json!({"name": "loopy"}))
        .await
        .unwrap();
    let tool_registry::ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    // The inner call is refused (step failed -> handover), not recursed.
    assert!(text.contains("HANDOVER"), "{text}");
    assert!(text.contains("call itself"), "{text}");
}

#[tokio::test]
async fn non_cyclic_nesting_deeper_than_max_depth_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    let n = crate::replay::MAX_WORKFLOW_DEPTH + 2;
    for i in 0..n {
        let mut wf = Workflow::new(format!("level{i}"));
        wf.description = "nested".into();
        wf.steps = if i + 1 < n {
            vec![Step::Tool(ToolStep {
                tool: "workflow_run".into(),
                input: json!({"name": format!("level{}", i + 1)}),
            })]
        } else {
            vec![Step::Shell(crate::format::Shell {
                command: "echo end".into(),
                shell: Some("sh".into()),
                cwd: None,
            })]
        };
        store.save(&wf).unwrap();
    }
    let registry = ToolRegistry::new();
    let capture = CaptureTool::new("shell_run");
    assert!(registry.register(capture.clone()));
    let h = Handles {
        desktop: None,
        browser: None,
        tools: registry.clone(),
    };
    let gate: Arc<dyn builtin_tools::ToolGate> = Arc::new(RecordingGate::default());
    for t in crate::tools(store.clone(), h, gate, ReplayOptions::instant()) {
        assert!(registry.register(t));
    }
    registry
        .get("workflow_run")
        .unwrap()
        .call(&ToolContext::default(), json!({"name": "level0"}))
        .await
        .unwrap();
    // The chain is cut at MAX_WORKFLOW_DEPTH: the innermost shell step never runs.
    // (A nested handover surfaces as text to its caller, so the outer report may
    // still read "completed"; what matters is that the deepest level is refused.)
    assert!(capture.inputs.lock().is_empty());
    // One level shallower than the limit does run to the end.
    let shallow = crate::replay::MAX_WORKFLOW_DEPTH - 1;
    registry
        .get("workflow_run")
        .unwrap()
        .call(
            &ToolContext::default(),
            json!({"name": format!("level{}", n - shallow)}),
        )
        .await
        .unwrap();
    assert_eq!(capture.inputs.lock().len(), 1);
}

#[tokio::test]
async fn workflow_cannot_recurse_through_script_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    let mut wf = Workflow::new("loopy");
    wf.description = "calls itself through a script".into();
    wf.steps = vec![Step::Tool(ToolStep {
        tool: "script_run".into(),
        input: json!({"code": "return await tools.call('workflow_run', Object.fromEntries([['name', 'loopy']]));"}),
    })];
    store.save(&wf).unwrap();
    let registry = ToolRegistry::new();
    let h = Handles {
        desktop: None,
        browser: None,
        tools: registry.clone(),
    };
    let gate: Arc<dyn builtin_tools::ToolGate> = Arc::new(RecordingGate::default());
    assert!(registry.register(builtin_tools::script::tool(
        registry.clone(),
        gate.clone(),
        script_runtime::Limits::default(),
    )));
    for t in crate::tools(store.clone(), h, gate, ReplayOptions::instant()) {
        assert!(registry.register(t));
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        registry
            .get("workflow_run")
            .unwrap()
            .call(&ToolContext::default(), json!({"name": "loopy"})),
    )
    .await
    .expect("must not recurse unbounded")
    .unwrap();
    let tool_registry::ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(text.contains("HANDOVER"), "{text}");
    // Either guard may fire first depending on feature unification.
    assert!(
        text.contains("would call itself") || text.contains("cannot call workflow_run"),
        "{text}"
    );
}

// ── control gate (SEC-06, replay half) ──────────────────────────────────────

/// Refuses every action and counts the asks.
#[derive(Default)]
struct RefusingControl {
    asked: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl builtin_tools::DesktopControlGate for RefusingControl {
    async fn before_action(&self, req: &builtin_tools::ControlRequest) -> Result<(), String> {
        self.asked.lock().push(req.action.clone());
        Err("the user has taken control".into())
    }
}

/// Admits, but a pause / takeover / override is in force by the time the step
/// would read or act: the replay must not touch the screen.
struct LatchedAdmit;

#[async_trait::async_trait]
impl builtin_tools::DesktopControlGate for LatchedAdmit {
    fn latch_refusal(&self) -> Option<String> {
        Some("The user stopped the bot; do not retry.".into())
    }
    async fn before_action(&self, _req: &builtin_tools::ControlRequest) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn a_replay_step_never_reads_or_acts_while_control_is_held_back() {
    let wf = screen_wf(vec![Step::DesktopAction(DesktopAction {
        app: None,
        window_title_pattern: Some("*App".into()),
        target: Some(desk_loc("button", "OK")),
        action: DesktopActionKind::Click,
        value: None,
        method: None,
    })]);
    let desktop = FakeDesktop::new(
        vec![window(1, "A - App")],
        vec![node(0, "button", "OK", 10.0, 10.0)],
    );
    let mut opts = ReplayOptions::instant();
    opts.control = Some(Arc::new(LatchedAdmit));
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(
        desktop.actions.lock().is_empty(),
        "no action was dispatched"
    );
}

fn refusing_opts() -> (ReplayOptions, Arc<RefusingControl>) {
    let c = Arc::new(RefusingControl::default());
    let mut o = ReplayOptions::instant();
    o.control = Some(c.clone());
    (o, c)
}

#[tokio::test]
async fn refusing_control_gate_blocks_every_desktop_action_kind() {
    for kind in [
        DesktopActionKind::Click,
        DesktopActionKind::Type,
        DesktopActionKind::SetValue,
        DesktopActionKind::Key,
    ] {
        let wf = screen_wf(vec![Step::DesktopAction(DesktopAction {
            app: None,
            window_title_pattern: Some("*App".into()),
            target: Some(desk_loc("button", "OK")),
            action: kind,
            value: Some("a".into()),
            method: None,
        })]);
        let desktop = FakeDesktop::new(
            vec![window(1, "A - App")],
            vec![node(0, "button", "OK", 10.0, 10.0)],
        );
        let (opts, control) = refusing_opts();
        let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
            .replay(&wf, &params(&json!({})), &RecordingGate::default())
            .await
            .unwrap();
        assert!(!report.is_completed(), "{kind:?}");
        assert!(
            desktop.actions.lock().is_empty(),
            "{kind:?} reached backend"
        );
        assert_eq!(control.asked.lock().len(), 1, "{kind:?}");
    }
}

#[tokio::test]
async fn refusing_control_gate_blocks_launch() {
    let wf = screen_wf(vec![Step::Launch(Launch {
        executable: "notepad.exe".into(),
        args: vec![],
    })]);
    let desktop = notepad_desktop();
    let (opts, _c) = refusing_opts();
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(desktop.launched.lock().is_empty());
}

#[tokio::test]
async fn refusing_control_gate_blocks_every_browser_action_and_navigate() {
    let page = "# Shop (https://shop.test/cart)\n  [e1] textbox \"Search\"\n  [e2] button \"Go\"\n";
    let bloc = |role: &str, name: &str, css: Option<&str>| BrowserLocator {
        role: role.into(),
        name: name.into(),
        fingerprint: fingerprint_hex(role, name),
        aria_label: None,
        css: css.map(Into::into),
        nth: None,
    };
    let mut cases: Vec<Step> = Vec::new();
    for (kind, target) in [
        (BrowserActionKind::Click, Some(bloc("button", "Go", None))),
        (
            BrowserActionKind::Fill,
            Some(bloc("textbox", "Search", None)),
        ),
        (BrowserActionKind::Select, Some(bloc("button", "Go", None))),
        (BrowserActionKind::Press, None),
        (BrowserActionKind::Back, None),
        // css fallback path (browser_eval)
        (BrowserActionKind::Click, Some(bloc("", "", Some("#buy")))),
    ] {
        cases.push(Step::BrowserAction(BrowserAction {
            url_pattern: None,
            target,
            action: kind,
            value: Some("x".into()),
        }));
    }
    cases.push(Step::Navigate(crate::format::Navigate {
        url: "https://shop.test/".into(),
        profile: None,
    }));
    for step in cases {
        let browser = FakeBrowser::new(page);
        let (opts, control) = refusing_opts();
        let report = Replayer::new(handles(None, Some(browser.clone())), opts)
            .replay(
                &screen_wf(vec![step.clone()]),
                &params(&json!({})),
                &RecordingGate::default(),
            )
            .await
            .unwrap();
        assert!(!report.is_completed(), "{step:?}");
        assert_eq!(control.asked.lock().len(), 1, "{step:?}");
        // Only the read-only css uniqueness probe may have touched the driver.
        let calls = browser.calls.lock();
        assert!(
            calls
                .iter()
                .all(|c| c.starts_with("eval:document.querySelectorAll")),
            "{step:?} mutated: {calls:?}"
        );
    }
}

#[tokio::test]
async fn missing_control_gate_fails_closed_for_acting_steps() {
    let wf = screen_wf(vec![Step::Launch(Launch {
        executable: "notepad.exe".into(),
        args: vec![],
    })]);
    let desktop = notepad_desktop();
    let opts = ReplayOptions {
        launch_settle: std::time::Duration::ZERO,
        ..ReplayOptions::default()
    };
    assert!(opts.control.is_none());
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(desktop.launched.lock().is_empty());
}
