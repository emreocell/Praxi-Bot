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
use crate::testkit::{
    CaptureTool, CoveringGate, FakeBrowser, FakeDesktop, RecordingGate, node, window,
};

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

struct BoundControl {
    inner: Arc<dyn builtin_tools::DesktopControlGate>,
    desktop: Option<Arc<dyn desktop_api::DesktopBackend>>,
}
#[async_trait::async_trait]
impl builtin_tools::DesktopControlGate for BoundControl {
    fn desktop_for_run(&self, _: &str) -> Option<Arc<dyn desktop_api::DesktopBackend>> {
        self.desktop.clone()
    }
    fn latch_refusal(&self) -> Option<String> {
        self.inner.latch_refusal()
    }
    fn observation_refused(&self) -> Option<String> {
        self.inner.observation_refused()
    }
    fn observation_epoch(&self) -> u64 {
        self.inner.observation_epoch()
    }
    async fn before_action(&self, req: &builtin_tools::ControlRequest) -> Result<(), String> {
        self.inner.before_action(req).await
    }
}
fn bound_options(handles: &Handles, mut opts: ReplayOptions) -> ReplayOptions {
    opts.control = opts.control.map(|inner| {
        Arc::new(BoundControl {
            inner,
            desktop: handles.desktop.clone(),
        }) as Arc<dyn builtin_tools::DesktopControlGate>
    });
    opts
}
fn replayer(handles: Handles, opts: ReplayOptions) -> Replayer {
    let opts = bound_options(&handles, opts);
    Replayer::new(handles, opts)
}
fn bound_tools(
    store: WorkflowStore,
    handles: Handles,
    gate: Arc<dyn builtin_tools::ToolGate>,
    opts: ReplayOptions,
) -> Vec<Arc<dyn tool_registry::Tool>> {
    let opts = bound_options(&handles, opts);
    crate::tools(store, handles, gate, opts)
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
        secret: None,
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
    let report = replayer(handles(Some(desktop.clone()), None), opts)
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
    let report = replayer(handles(Some(desktop.clone()), None), opts)
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
    let report = replayer(
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
    let report = replayer(handles(Some(desktop), None), ReplayOptions::instant())
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
            secret: None,
        }),
        Step::BrowserAction(BrowserAction {
            url_pattern: None,
            target: Some(bloc("button", "Go")),
            action: BrowserActionKind::Click,
            value: None,
            secret: None,
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
    let rp = replayer(h, ReplayOptions::instant());
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
    let report = replayer(
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
async fn success_checks_require_computer_read_admission_before_backend_access() {
    let desktop = notepad_desktop();
    let browser = FakeBrowser::new("needle");
    for (check, tool) in [
        (
            SuccessCheck::WindowTitle {
                pattern: "*".into(),
            },
            "desktop_list_apps",
        ),
        (
            SuccessCheck::DesktopText {
                window_title_pattern: None,
                contains: "needle".into(),
            },
            "desktop_get_state",
        ),
        (
            SuccessCheck::BrowserText {
                contains: "needle".into(),
            },
            "browser_snapshot",
        ),
    ] {
        let mut wf = Workflow::new("read-admission");
        wf.success_check = Some(check);
        let gate = RecordingGate::denying(tool);
        let rp = replayer(
            handles(Some(desktop.clone()), Some(browser.clone())),
            ReplayOptions::instant(),
        );
        let report = rp
            .replay(&wf, &params(&json!({})), gate.as_ref())
            .await
            .unwrap();
        assert!(
            report
                .handover()
                .unwrap()
                .reason
                .detail()
                .contains("permission denied")
        );
        assert_eq!(gate.tools(), vec![tool]);
        assert!(browser.calls.lock().is_empty());
    }
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
    let rp = replayer(
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
        secret: None,
    })];
    let rp = replayer(
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
        secret: None,
    })];
    let r = replayer(
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
    let tools = bound_tools(
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
    let denied = bound_tools(
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
    let rp = replayer(h, ReplayOptions::instant());
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
    let report = replayer(h, ReplayOptions::instant())
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
    for t in bound_tools(
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
    for t in bound_tools(store.clone(), h, gate, ReplayOptions::instant()) {
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
    for t in bound_tools(store.clone(), h, gate, ReplayOptions::instant()) {
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
        secret: None,
    })]);
    let desktop = FakeDesktop::new(
        vec![window(1, "A - App")],
        vec![node(0, "button", "OK", 10.0, 10.0)],
    );
    let mut opts = ReplayOptions::instant();
    opts.control = Some(Arc::new(LatchedAdmit));
    let report = replayer(handles(Some(desktop.clone()), None), opts)
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
            secret: None,
        })]);
        let desktop = FakeDesktop::new(
            vec![window(1, "A - App")],
            vec![node(0, "button", "OK", 10.0, 10.0)],
        );
        let (opts, control) = refusing_opts();
        let report = replayer(handles(Some(desktop.clone()), None), opts)
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
    let report = replayer(handles(Some(desktop.clone()), None), opts)
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
            secret: None,
        }));
    }
    cases.push(Step::Navigate(crate::format::Navigate {
        url: "https://shop.test/".into(),
        profile: None,
    }));
    for step in cases {
        let browser = FakeBrowser::new(page);
        let (opts, control) = refusing_opts();
        let report = replayer(handles(None, Some(browser.clone())), opts)
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
    let report = replayer(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(desktop.launched.lock().is_empty());
}

#[tokio::test]
async fn replay_missing_run_desktop_never_uses_local_default() {
    let desktop = notepad_desktop();
    // AllowAllControl supplies no per-run desktop, despite a default backend.
    let opts = ReplayOptions::instant();
    let wf = screen_wf(vec![Step::Launch(Launch {
        executable: "notepad.exe".into(),
        args: vec![],
    })]);
    let report = Replayer::new(handles(Some(desktop.clone()), None), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(desktop.launched.lock().is_empty());
}

#[tokio::test]
async fn remote_pc_replay_cannot_navigate_host_browser() {
    struct NoBrowser;
    #[async_trait::async_trait]
    impl builtin_tools::DesktopControlGate for NoBrowser {
        fn browser_available_for_run(&self, _: &str) -> bool {
            false
        }
        async fn before_action(&self, _: &builtin_tools::ControlRequest) -> Result<(), String> {
            Ok(())
        }
    }
    let browser = FakeBrowser::new("# Page (https://example.test)");
    let mut opts = ReplayOptions::instant();
    opts.control = Some(Arc::new(NoBrowser));
    let wf = screen_wf(vec![Step::Navigate(crate::format::Navigate {
        url: "https://example.test".into(),
        profile: None,
    })]);
    let report = Replayer::new(handles(None, Some(browser.clone())), opts)
        .replay(&wf, &params(&json!({})), &RecordingGate::default())
        .await
        .unwrap();
    assert!(!report.is_completed());
    assert!(browser.calls.lock().is_empty());
}

// ---- run-scoped control grant: replay goes through the guard ----

fn typed_step(kind: DesktopActionKind, value: &str) -> Workflow {
    screen_wf(vec![Step::DesktopAction(DesktopAction {
        app: None,
        window_title_pattern: Some("*App".into()),
        target: Some(desk_loc("button", "OK")),
        action: kind,
        value: Some(value.into()),
        method: None,
        secret: None,
    })])
}

fn named_desktop() -> Arc<FakeDesktop> {
    let desktop = FakeDesktop::new(
        vec![window(1, "A - App")],
        vec![node(0, "button", "OK", 10.0, 10.0)],
    );
    *desktop.input.lock() = Some(FakeDesktop::notepad_target());
    desktop
}

#[tokio::test]
async fn a_key_step_that_first_clicks_an_element_is_never_covered() {
    let desktop = named_desktop();
    let gate = CoveringGate::default();
    let report = replayer(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(
        &typed_step(DesktopActionKind::Key, "ctrl+s"),
        &params(&json!({})),
        &gate,
    )
    .await
    .unwrap();
    assert!(report.is_completed());
    assert!(gate.requests.lock()[0].control.is_none(), "it asks");
    assert!(
        desktop.guarded.lock().is_empty(),
        "nothing took the guarded path"
    );
    assert_eq!(desktop.actions.lock().len(), 2, "click and key, unguarded");
}

#[tokio::test]
async fn replay_passes_through_the_guard_when_a_grant_covers_the_step() {
    for (kind, value, guarded) in [
        (DesktopActionKind::Type, "hello", 2),
        (DesktopActionKind::SetValue, "hello", 1),
    ] {
        let desktop = named_desktop();
        let gate = CoveringGate::default();
        let report = replayer(
            handles(Some(desktop.clone()), None),
            ReplayOptions::instant(),
        )
        .replay(&typed_step(kind, value), &params(&json!({})), &gate)
        .await
        .unwrap();
        assert!(report.is_completed(), "{kind:?}");
        let seen = gate.requests.lock();
        let control = seen[0].control.as_ref().expect("a target was resolved");
        assert!(control.eligible(), "{kind:?}: {control:?}");
        assert_eq!(control.program_name(), "notepad.exe");
        assert_eq!(
            desktop.guarded.lock().len(),
            guarded,
            "{kind:?}: every input action of the step, the focusing click included, was guarded"
        );
        assert_eq!(desktop.actions.lock().len(), guarded, "{kind:?}");
    }
}

struct Swap(Arc<FakeDesktop>);
#[async_trait::async_trait]
impl builtin_tools::ToolGate for Swap {
    async fn authorize(&self, _: &builtin_tools::GateRequest) -> Result<(), String> {
        Ok(())
    }
    async fn authorize_control(
        &self,
        req: &builtin_tools::GateRequest,
    ) -> Result<Option<tool_registry::ControlIdentity>, String> {
        let id = req.control.as_ref().map(|t| t.identity.clone());
        self.0.input.lock().as_mut().unwrap().identity.pid += 1;
        Ok(id)
    }
}

/// [`Swap`] that records the run-wide withdrawals.
struct SwapSpy(Swap, parking_lot::Mutex<Vec<String>>);
#[async_trait::async_trait]
impl builtin_tools::ToolGate for SwapSpy {
    async fn authorize(&self, r: &builtin_tools::GateRequest) -> Result<(), String> {
        self.0.authorize(r).await
    }
    async fn authorize_control(
        &self,
        r: &builtin_tools::GateRequest,
    ) -> Result<Option<tool_registry::ControlIdentity>, String> {
        self.0.authorize_control(r).await
    }
    fn revoke_control(&self, run_id: &str) {
        self.1.lock().push(run_id.to_owned());
    }
}

#[tokio::test]
async fn a_guarded_failure_in_replay_withdraws_the_runs_control_grants() {
    let desktop = named_desktop();
    let gate = SwapSpy(Swap(desktop.clone()), parking_lot::Mutex::default());
    let report = replayer(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(
        &typed_step(DesktopActionKind::Type, "hello"),
        &params(&json!({})),
        &gate,
    )
    .await
    .unwrap();
    assert!(!report.is_completed());
    assert_eq!(gate.1.lock().len(), 1, "the grant was withdrawn once");
}

#[tokio::test]
async fn replay_does_not_send_when_the_window_is_not_the_granted_program() {
    let desktop = named_desktop();
    // Another program owns the window by the time the guard runs.
    let report = replayer(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(
        &typed_step(DesktopActionKind::Type, "powershell -w hidden"),
        &params(&json!({})),
        &Swap(desktop.clone()),
    )
    .await
    .unwrap();
    assert!(!report.is_completed());
    assert!(desktop.actions.lock().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn replay_without_a_grant_does_not_use_the_guarded_path() {
    let desktop = named_desktop();
    let report = replayer(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(
        &typed_step(DesktopActionKind::Type, "hello"),
        &params(&json!({})),
        &RecordingGate::default(),
    )
    .await
    .unwrap();
    assert!(report.is_completed());
    assert!(desktop.guarded.lock().is_empty());
    assert_eq!(desktop.actions.lock().len(), 2);
}

#[tokio::test]
async fn a_replayed_win_chord_has_no_eligible_target() {
    let desktop = named_desktop();
    let gate = CoveringGate::default();
    replayer(
        handles(Some(desktop.clone()), None),
        ReplayOptions::instant(),
    )
    .replay(
        // No element: the keys go to the live focus, the covered shape of a key step.
        &screen_wf(vec![Step::DesktopAction(DesktopAction {
            app: None,
            window_title_pattern: Some("*App".into()),
            target: None,
            action: DesktopActionKind::Key,
            value: Some("win+r".into()),
            method: None,
            secret: None,
        })]),
        &params(&json!({})),
        &gate,
    )
    .await
    .unwrap();
    let seen = gate.requests.lock();
    let control = seen[0].control.as_ref().unwrap();
    assert_eq!(control.refusal.as_deref(), Some("chord"));
    assert!(
        desktop.guarded.lock().is_empty(),
        "not covered, so not guarded"
    );
}

// ---------------------------------------------------------------- secrets

mod secret_steps {
    use super::*;
    use builtin_tools::browser::FieldTarget;
    use secret_store::{FillSecrets, MemoryStore, SecretStore};

    const VALUE: &str = "hunter2-Correct-Horse-9!";
    const GITHUB: &str = "https://github.com";
    const PAGE: &str = "# Login (https://github.com/login)\n  [e1] textbox \"Password\"\n";

    fn secret_step(name: &str) -> Step {
        Step::BrowserAction(BrowserAction {
            url_pattern: Some("https://github.com/*".into()),
            target: Some(BrowserLocator {
                role: "textbox".into(),
                name: "Password".into(),
                fingerprint: fingerprint_hex("textbox", "Password"),
                aria_label: None,
                css: None,
                nth: None,
            }),
            action: BrowserActionKind::Fill,
            value: None,
            secret: Some(name.into()),
        })
    }

    fn secrets() -> Arc<FillSecrets> {
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let secrets = Arc::new(FillSecrets::in_memory(store));
        secrets
            .upsert(
                "login/github",
                Some(VALUE),
                Some(&[GITHUB.to_owned()]),
                None,
            )
            .unwrap();
        secrets
    }

    fn rig(frame: &str) -> (Arc<FakeBrowser>, Arc<FillSecrets>, Replayer) {
        let browser = FakeBrowser::new(PAGE);
        browser.targets.lock().insert(
            "e1".into(),
            FieldTarget {
                name: "Password".into(),
                role: "textbox".into(),
                frame_origin: Some(frame.into()),
                top_origin: Some(frame.into()),
                sensitive: false,
            },
        );
        let secrets = secrets();
        let mut opts = ReplayOptions::instant();
        opts.fill_secrets = Some(Arc::clone(&secrets));
        let h = handles(None, Some(browser.clone()));
        (browser, secrets, replayer(h, opts))
    }

    fn wf(step: Step) -> Workflow {
        let mut w = Workflow::new("login");
        w.steps = vec![step];
        w
    }

    /// Guard: a recorded step keeps the reference; replay goes through the
    /// gate with the same tool and the reference (never the value), then types.
    #[tokio::test]
    async fn replay_asks_through_the_gate_and_types_the_stored_value() {
        let (browser, _s, rp) = rig(GITHUB);
        let gate = RecordingGate::default();
        let report = rp
            .replay(&wf(secret_step("login/github")), &params(&json!({})), &gate)
            .await
            .unwrap();
        assert!(report.is_completed(), "{report:?}");
        let reqs = gate.requests.lock().clone();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].tool, "browser_fill");
        assert_eq!(
            reqs[0].input,
            json!({"ref": "e1", "secret": "login/github"})
        );
        assert!(
            !serde_json::to_string(&reqs[0].input)
                .unwrap()
                .contains(VALUE)
        );
        assert_eq!(browser.typed_secrets.lock().as_slice(), [VALUE]);
        // The report (what a handover or the model reads) never carries it.
        assert!(
            !format!("{report:?}").contains("Correct-Horse"),
            "{report:?}"
        );
    }

    /// Guard: replay re-checks the LIVE frame origin; a drifted page types nothing.
    #[tokio::test]
    async fn replay_refuses_a_frame_the_owner_did_not_allow() {
        let (browser, _s, rp) = rig("https://evil.test");
        let gate = RecordingGate::default();
        let report = rp
            .replay(&wf(secret_step("login/github")), &params(&json!({})), &gate)
            .await
            .unwrap();
        assert!(!report.is_completed());
        assert!(browser.typed_secrets.lock().is_empty());
        assert!(
            format!("{report:?}").contains("not allowed on https://evil.test"),
            "{report:?}"
        );
    }

    /// Guard: a deleted secret is a clear error before anything is asked.
    #[tokio::test]
    async fn replay_with_a_missing_secret_is_a_clear_error_and_asks_nothing() {
        let (browser, secrets, rp) = rig(GITHUB);
        secrets.delete("login/github").unwrap();
        let gate = RecordingGate::default();
        let report = rp
            .replay(&wf(secret_step("login/github")), &params(&json!({})), &gate)
            .await
            .unwrap();
        assert!(!report.is_completed());
        let text = format!("{report:?}");
        assert!(
            text.contains("owner has no secret with that name"),
            "{text}"
        );
        assert!(gate.requests.lock().is_empty());
        assert!(browser.typed_secrets.lock().is_empty());
    }

    /// Guard: a refused gate (the owner said no) types nothing.
    #[tokio::test]
    async fn replay_types_nothing_when_the_gate_refuses() {
        let (browser, _s, rp) = rig(GITHUB);
        let gate = RecordingGate::denying("browser_fill");
        let report = rp
            .replay(
                &wf(secret_step("login/github")),
                &params(&json!({})),
                &*gate,
            )
            .await
            .unwrap();
        assert!(!report.is_completed());
        assert!(browser.typed_secrets.lock().is_empty());
    }

    /// Guard: no secrets service configured is a clear error.
    #[tokio::test]
    async fn replay_without_a_secret_service_refuses() {
        let browser = FakeBrowser::new(PAGE);
        let rp = replayer(
            handles(None, Some(browser.clone())),
            ReplayOptions::instant(),
        );
        let gate = RecordingGate::default();
        let report = rp
            .replay(&wf(secret_step("login/github")), &params(&json!({})), &gate)
            .await
            .unwrap();
        assert!(!report.is_completed());
        assert!(format!("{report:?}").contains("no secrets are configured"));
    }

    /// Guard: a desktop secret step is kept but never replayed blind.
    #[tokio::test]
    async fn a_desktop_secret_step_is_refused_at_replay() {
        let gate = RecordingGate::default();
        let step = Step::DesktopAction(DesktopAction {
            app: None,
            window_title_pattern: None,
            target: Some(desk_loc("edit", "Text editor")),
            action: DesktopActionKind::Type,
            value: None,
            method: None,
            secret: Some("login/github".into()),
        });
        let h = handles(Some(notepad_desktop()), None);
        let rp = replayer(h, ReplayOptions::instant());
        let report = rp
            .replay(&wf(step), &params(&json!({})), &gate)
            .await
            .unwrap();
        assert!(!report.is_completed());
        assert!(format!("{report:?}").contains("desktop secret steps are not replayed"));
        assert!(gate.requests.lock().is_empty());
    }

    /// Guard: the format only allows a secret where it makes sense.
    #[test]
    fn validation_rejects_a_secret_in_the_wrong_place() {
        let ok = wf(secret_step("login/github"));
        ok.validate().unwrap();
        // A value next to a secret.
        let mut bad = ok.clone();
        if let Step::BrowserAction(b) = &mut bad.steps[0] {
            b.value = Some("x".into());
        }
        assert!(bad.validate().is_err());
        // A secret on a click.
        let mut bad = ok.clone();
        if let Step::BrowserAction(b) = &mut bad.steps[0] {
            b.action = BrowserActionKind::Click;
        }
        assert!(bad.validate().is_err());
        // A reserved / malformed name.
        for name in ["composio/gmail", "fill/x", "a b", ""] {
            assert!(wf(secret_step(name)).validate().is_err(), "{name}");
        }
        // The file holds the reference only.
        let json = ok.to_json().unwrap();
        assert!(json.contains("login/github") && !json.contains("\"value\""));
    }
}

#[tokio::test]
async fn navigate_replay_encodes_query_values_and_redacts_secret_params_on_handover() {
    let page = "# Search (https://shop.test/search)\n";
    let browser = FakeBrowser::new(page);
    let mut wf = Workflow::new("shop_search");
    wf.params = vec![
        Param {
            name: "q".into(),
            ty: ParamType::String,
            default: Some("red shoes".into()),
            description: String::new(),
        },
        Param {
            name: "token".into(),
            ty: ParamType::Secret,
            default: None,
            description: String::new(),
        },
    ];
    wf.steps = vec![
        Step::Navigate(crate::format::Navigate {
            url: "https://shop.test/search?q={q}&token={token}#top".into(),
            profile: None,
        }),
        // Force a handover after Navigate so we can inspect PartialState.params.
        Step::Agent(AgentStep {
            instruction: "done".into(),
        }),
    ];

    let h = handles(None, Some(browser.clone()));
    let gate = RecordingGate::default();
    let report = replayer(h.clone(), ReplayOptions::instant())
        .replay(
            &wf,
            &params(&json!({"q": "blue & green=100% 🚀", "token": "super-secret-value"})),
            &gate,
        )
        .await
        .unwrap();

    assert_eq!(
        browser.calls.lock().as_slice(),
        [
            "open:https://shop.test/search?q=blue+%26+green%3D100%25+%F0%9F%9A%80&token=super-secret-value#top"
        ]
    );
    let handover = report.handover().expect("agent step hands over");
    assert_eq!(
        handover
            .partial_state
            .params
            .get("token")
            .map(String::as_str),
        Some(secret_store::REDACTED)
    );
    assert!(!format!("{report:?}").contains("super-secret-value"));

    // Model-facing workflow_run tool refuses workflows with ParamType::Secret.
    let dir = tempfile::tempdir().unwrap();
    let store = WorkflowStore::new(dir.path());
    store.save(&wf).unwrap();
    let run_tool = bound_tools(store, h, Arc::new(gate), ReplayOptions::instant())
        .into_iter()
        .find(|t| t.spec().name == crate::RUN_TOOL_NAME)
        .unwrap();
    let err = run_tool
        .call(
            &ToolContext::default(),
            json!({"name": "shop_search", "params": {"q": "x", "token": "y"}}),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
}
