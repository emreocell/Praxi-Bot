//! The run-scoped control grant: after untrusted content, one
//! card per program lets a run type into that one program, and nothing else.

#![allow(clippy::too_many_lines, clippy::unnecessary_wraps)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use permission_engine::{Level, Policy, Rule};
use protocol::event::ApprovalNeeded;
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, ControlIdentity, ControlKind, ControlTarget, Effect, Platform, Tool, ToolContext,
    ToolError, ToolOutput, ToolSpec,
};

use crate::tests::{
    Env, Fake, MACHINE_A, MACHINE_B, calls, cfg, machine_provider, profile, setup, text, until,
    with_class,
};
use crate::{AgentLoop, ApprovalScope, LoopError};

/// A desktop tool whose target a test scripts. It records the guard the loop
/// handed it, which is how "the backend would check this program" is observed.
pub(crate) struct ControlTool {
    spec: ToolSpec,
    pub count: AtomicUsize,
    /// One target per `control_target` call; the last one repeats.
    script: Mutex<VecDeque<Option<ControlTarget>>>,
    last: Mutex<Option<ControlTarget>>,
    pub guards: Mutex<Vec<Option<ControlIdentity>>>,
    always_ask: bool,
    /// Fail the nth call (1-based, 0 = never) when it carries a guard: a guard
    /// refusal at the backend.
    pub fail_call: AtomicUsize,
}

impl ControlTool {
    pub(crate) fn new(name: &str, script: Vec<Option<ControlTarget>>) -> Arc<Self> {
        Arc::new(Self {
            spec: ToolSpec {
                name: name.to_owned(),
                description: format!("test {name}"),
                input_schema: json!({"type": "object"}),
                effect: Effect::Write,
                category: PermissionCategory::Desktop,
                platforms: vec![Platform::All],
                content: builtin_tools::classify(name).expect("a built-in class"),
            },
            count: AtomicUsize::new(0),
            script: Mutex::new(script.into()),
            last: Mutex::new(None),
            guards: Mutex::new(Vec::new()),
            always_ask: false,
            fail_call: AtomicUsize::new(0),
        })
    }

    fn always_asking(name: &str, script: Vec<Option<ControlTarget>>) -> Arc<Self> {
        let mut t = Arc::try_unwrap(Self::new(name, script)).ok().unwrap();
        t.always_ask = true;
        Arc::new(t)
    }
}

#[async_trait]
impl Tool for ControlTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn control_target(&self, _: &ToolContext, _: &Value) -> Option<ControlTarget> {
        let mut script = self.script.lock();
        let mut last = self.last.lock();
        if let Some(next) = script.pop_front() {
            *last = next;
        }
        last.clone()
    }

    fn always_ask(&self) -> bool {
        self.always_ask
    }

    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        self.guards.lock().push(ctx.control_guard.clone());
        if ctx.control_guard.is_some() && self.fail_call.load(Ordering::SeqCst) == n {
            return Err(ToolError::Execution(
                "stopped by the guard (foreground_changed); 24 of 60 text units had already been typed"
                    .into(),
            ));
        }
        Ok(ToolOutput::text("ok"))
    }
}

fn identity(pid: u32, start: u64) -> ControlIdentity {
    ControlIdentity {
        image_path: "C:\\Windows\\System32\\notepad.exe".into(),
        pid,
        start_time: start,
    }
}

fn target_for(kind: ControlKind, refusal: Option<&str>) -> Option<ControlTarget> {
    Some(ControlTarget {
        kind,
        identity: identity(10, 99),
        window_title: "Untitled - Notepad".into(),
        refusal: refusal.map(str::to_owned),
    })
}

fn ok(kind: ControlKind) -> Option<ControlTarget> {
    target_for(kind, None)
}

fn read_tool() -> Arc<crate::tests::TestTool> {
    Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "page text",
    ))
}

fn control_loop(env: &Env, fake: &Arc<Fake>, waiver: bool, with_computer: bool) -> AgentLoop {
    let mut extra = String::new();
    if waiver {
        extra.push_str("[permissions]\nallow_egress_after_reads = true\n");
    }
    extra.push_str("[runtime]\ncomputer = \"host\"\n");
    let lp = crate::tests::make_loop(
        &env.storage,
        env.bot,
        fake,
        &[],
        profile(&extra),
        cfg(Policy::full()),
    );
    lp.sh.tools.register(read_tool() as Arc<dyn Tool>);
    // A desktop observation reads untrusted text too.
    lp.sh.tools.register(Arc::new(with_class(
        "desktop_get_state",
        Effect::Read,
        PermissionCategory::Desktop,
        builtin_tools::classify("desktop_get_state").unwrap(),
        "window text",
    )) as Arc<dyn Tool>);
    // A computer whose id is not a machine fingerprint has no identity the
    // grant could be scoped to.
    let id = if with_computer {
        MACHINE_A
    } else {
        "not-a-machine-fingerprint"
    };
    lp.set_computer_provider(machine_provider(id, Some("host"), None, false));
    lp
}

fn register(lp: &AgentLoop, tools: &[&Arc<ControlTool>]) {
    for t in tools {
        lp.sh.tools.register(Arc::clone(t) as Arc<dyn Tool>);
    }
}

fn control_card(a: &ApprovalNeeded) -> Value {
    a.details
        .as_ref()
        .and_then(|d| d.get("control_grant"))
        .cloned()
        .unwrap_or(Value::Null)
}

async fn next_card(rx: &mut tokio::sync::broadcast::Receiver<crate::StreamItem>) -> ApprovalNeeded {
    match until(rx, |e| matches!(e, Event::ApprovalNeeded(_))).await {
        Event::ApprovalNeeded(a) => a,
        _ => unreachable!(),
    }
}

/// Answer every card with `pick(card)` until the run ends; returns the cards.
async fn drive(
    lp: &AgentLoop,
    env: &Env,
    rx: &mut tokio::sync::broadcast::Receiver<crate::StreamItem>,
    run: protocol::RunId,
    mut pick: impl FnMut(&ApprovalNeeded) -> ApprovalScope,
) -> Vec<ApprovalNeeded> {
    let mut cards = Vec::new();
    // `wait` consumes the run's task, so it is polled once, beside the cards.
    let mut waiting = Box::pin(lp.wait(run));
    loop {
        tokio::select! {
            state = &mut waiting => {
                assert_eq!(state.unwrap(), RunState::Succeeded);
                return cards;
            }
            a = next_card(rx) => {
                let scope = pick(&a);
                lp.approve(a.call_id, scope, env.user).unwrap();
                cards.push(a);
            }
        }
    }
}

/// `fs_read` (the untrusted read), then one call per `(id, tool)`.
fn script_type_then(more: &[(&str, &str)]) -> Arc<Fake> {
    let mut replies = vec![calls(&[("c0", "fs_read", json!({"path": "page.html"}))])];
    for (id, name) in more {
        replies.push(calls(&[(
            id,
            name,
            json!({"index": 1, "gen": 1, "text": "a", "keys": "a"}),
        )]));
    }
    replies.push(text("done"));
    Fake::new(replies)
}

// --------------------------------------------------------------- usability

#[tokio::test]
async fn granted_app_types_and_presses_keys_without_cards_with_the_waiver_off() {
    let env = setup();
    let fake = script_type_then(&[
        ("c1", "desktop_type"),
        ("c2", "desktop_key"),
        ("c3", "desktop_type"),
        ("c4", "desktop_set_value"),
        ("c6", "desktop_key"),
    ]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let key = ControlTool::new("desktop_key", vec![ok(ControlKind::Key)]);
    let set = ControlTool::new("desktop_set_value", vec![ok(ControlKind::SetValue)]);
    register(&lp, &[&ty, &key, &set]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |_| ApprovalScope::RunControl).await;
    assert_eq!(cards.len(), 1, "one card for the whole task");
    let c = control_card(&cards[0]);
    assert_eq!(c["offered"], true);
    assert_eq!(c["program"], "notepad.exe");
    assert!(
        cards[0].details.as_ref().unwrap()["reason"]
            .as_str()
            .unwrap()
            .contains("starts a program")
    );
    assert_eq!(ty.count.load(Ordering::SeqCst), 2);
    assert_eq!(key.count.load(Ordering::SeqCst), 2);
    assert_eq!(set.count.load(Ordering::SeqCst), 1);
    // Every covered call, the first (approved with the grant) included, carries
    // the guard the backend checks right before it sends.
    for guards in [&ty.guards, &key.guards, &set.guards] {
        for g in guards.lock().iter() {
            assert_eq!(g.as_ref(), Some(&identity(10, 99)));
        }
    }
}

/// The ten desktop calls of the card's Notepad example after one `fs_read`:
/// an observation, launch, then `type`, `key`, `type`, `key`, `set_value`, `key`, `type`, `key` (the
/// last two land in the Save dialog).
fn notepad_script() -> Arc<Fake> {
    let mut replies = vec![calls(&[("c0", "fs_read", json!({"path": "page.html"}))])];
    for (id, name) in [
        ("c00", "desktop_get_state"),
        ("c1", "desktop_launch"),
        ("c2", "desktop_type"),
        ("c3", "desktop_key"),
        ("c4", "desktop_type"),
        ("c5", "desktop_key"),
        ("c6", "desktop_set_value"),
        ("c7", "desktop_key"),
        ("c8", "desktop_type"),
        ("c9", "desktop_key"),
    ] {
        replies.push(calls(&[(
            id,
            name,
            json!({"index": 1, "gen": 1, "text": "a", "keys": "a", "value": "a",
                   "executable": "notepad.exe", "args": []}),
        )]));
    }
    replies.push(text("done"));
    Fake::new(replies)
}

async fn notepad_cards(identity_known: bool) -> usize {
    let env = setup();
    let fake = notepad_script();
    let lp = control_loop(&env, &fake, false, true);
    let t = |k, refusals: &[Option<&str>]| -> Vec<Option<ControlTarget>> {
        if identity_known {
            refusals.iter().map(|r| target_for(k, *r)).collect()
        } else {
            vec![None]
        }
    };
    let launch = ControlTool::new("desktop_launch", t(ControlKind::Type, &[None]));
    let ty = ControlTool::new(
        "desktop_type",
        t(ControlKind::Type, &[None, None, Some("dialog")]),
    );
    let key = ControlTool::new(
        "desktop_key",
        t(ControlKind::Key, &[None, None, None, Some("dialog")]),
    );
    let set = ControlTool::new("desktop_set_value", t(ControlKind::SetValue, &[None]));
    register(&lp, &[&launch, &ty, &key, &set]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |a| {
        if control_card(a)["offered"] == true {
            ApprovalScope::RunControl
        } else {
            ApprovalScope::Once
        }
    })
    .await;
    assert_eq!(launch.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        ty.count.load(Ordering::SeqCst)
            + key.count.load(Ordering::SeqCst)
            + set.count.load(Ordering::SeqCst),
        8,
        "every call of the task ran"
    );
    cards.len()
}

#[tokio::test]
async fn notepad_task_shows_few_cards_where_the_backend_names_the_program() {
    // launch (a card of its own, never covered), the first type (the grant),
    // then the two calls that land in the Save dialog (a dialog always asks).
    assert_eq!(notepad_cards(true).await, 4);
}

#[tokio::test]
async fn the_same_notepad_task_without_process_identity_asks_exactly_as_before() {
    // launch + type + key + type + key + set_value + key + type + key: every
    // call of an executor tool asks after untrusted content (the rule of #89).
    assert_eq!(notepad_cards(false).await, 9);
}

// --------------------------------------------------- the grant is not too wide

/// One grant on `desktop_type`, then `follow` is called with the tool name and
/// target script; returns the cards after the first.
async fn after_a_grant(
    follow_name: &str,
    follow_script: Vec<Option<ControlTarget>>,
) -> (
    Vec<ApprovalNeeded>,
    Arc<ControlTool>,
    Arc<ControlTool>,
    AgentLoop,
    Env,
) {
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", follow_name)]);
    let lp = control_loop(&env, &fake, false, true);
    // The first call is always a typing call. When the follow-up is a typing
    // call too, one tool carries both targets.
    let follow = ControlTool::new(follow_name, follow_script);
    let ty = if follow_name == "desktop_type" {
        Arc::clone(&follow)
    } else {
        ControlTool::new("desktop_type", vec![ok(ControlKind::Type)])
    };
    register(&lp, &[&ty, &follow]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |a| {
        if control_card(a)["offered"] == true {
            ApprovalScope::RunControl
        } else {
            ApprovalScope::Once
        }
    })
    .await;
    (cards, ty, follow, lp, env)
}

#[tokio::test]
async fn control_grant_does_not_cover_win_r_or_any_super_chord() {
    let (cards, _ty, key, _lp, _env) = after_a_grant(
        "desktop_key",
        vec![target_for(ControlKind::Key, Some("chord"))],
    )
    .await;
    assert_eq!(
        cards.len(),
        2,
        "the chord asked although the program is granted"
    );
    let c = control_card(&cards[1]);
    assert_eq!(c["offered"], false);
    assert_eq!(c["refusal"], "chord");
    assert_eq!(
        key.guards.lock().as_slice(),
        &[None],
        "approved once, unguarded, not covered"
    );
}

#[tokio::test]
async fn control_grant_for_notepad_does_not_cover_typing_into_a_terminal() {
    let mut cmd = target_for(ControlKind::Type, Some("terminal")).unwrap();
    cmd.identity = ControlIdentity {
        image_path: "C:\\Windows\\System32\\cmd.exe".into(),
        pid: 77,
        start_time: 5,
    };
    let (cards, ..) = after_a_grant("desktop_type", vec![ok(ControlKind::Type), Some(cmd)]).await;
    assert_eq!(cards.len(), 2);
    assert_eq!(control_card(&cards[1])["refusal"], "terminal");
    assert_eq!(control_card(&cards[1])["program"], "cmd.exe");
}

#[tokio::test]
async fn password_field_and_file_dialog_in_a_granted_app_still_ask() {
    for reason in ["password_field", "dialog", "elevated"] {
        let (cards, ..) = after_a_grant(
            "desktop_type",
            vec![
                ok(ControlKind::Type),
                target_for(ControlKind::Type, Some(reason)),
            ],
        )
        .await;
        assert_eq!(cards.len(), 2, "{reason}");
        assert_eq!(control_card(&cards[1])["refusal"], reason);
        assert_eq!(control_card(&cards[1])["offered"], false);
    }
}

#[tokio::test]
async fn second_process_of_the_same_program_asks_again() {
    for other in [identity(11, 99), identity(10, 100)] {
        // A new pid, or the same pid recycled (a different start time).
        let second = Some(ControlTarget {
            kind: ControlKind::Type,
            identity: other,
            window_title: "Untitled - Notepad".into(),
            refusal: None,
        });
        let (cards, ..) = after_a_grant("desktop_type", vec![ok(ControlKind::Type), second]).await;
        assert_eq!(cards.len(), 2);
        assert_eq!(
            control_card(&cards[1])["offered"],
            true,
            "a new program may be granted on its own card"
        );
    }
}

#[tokio::test]
async fn control_grant_never_covers_launch_shell_or_script() {
    for name in ["desktop_launch", "shell_run", "script_run"] {
        // Even a tool that (wrongly) supplies an eligible target is not covered.
        let (cards, _ty, other, ..) = after_a_grant(name, vec![ok(ControlKind::Type)]).await;
        assert_eq!(cards.len(), 2, "{name} asked despite the grant");
        assert_eq!(other.guards.lock().as_slice(), &[None], "{name}");
        // Its own card never offers the grant.
        assert_eq!(control_card(&cards[1])["offered"], Value::Null, "{name}");
    }
}

#[tokio::test]
async fn deny_and_always_ask_still_win_over_a_control_grant() {
    // always_ask: asks although the program is granted, and is not grantable.
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "desktop_key")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let key = ControlTool::always_asking("desktop_key", vec![ok(ControlKind::Key)]);
    register(&lp, &[&ty, &key]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    let second = next_card(&mut rx).await;
    assert_eq!(second.tool_name, "desktop_key");
    assert_eq!(control_card(&second)["offered"], Value::Null);
    assert!(matches!(
        lp.approve(second.call_id, ApprovalScope::RunControl, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.approve(second.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    // deny: a category deny that lands after the grant wins.
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "web_extract")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let fetch = Arc::new(with_class(
        "web_extract",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "x",
    ));
    register(&lp, &[&ty]);
    lp.sh.tools.register(Arc::clone(&fetch) as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    let pause = next_card(&mut rx).await; // the fetch asks: the run is parked here
    assert_eq!(pause.tool_name, "web_extract");
    let ctx = lp.sh.runs.lock().get(&run).cloned().unwrap();
    let t = ok(ControlKind::Type).unwrap();
    let input = json!({"text": "a"});
    let d = |lp: &AgentLoop| {
        lp.sh.decide_with_target(
            &ctx,
            "desktop_type",
            Effect::Write,
            PermissionCategory::Desktop,
            &input,
            Some(&t),
        )
    };
    assert_eq!(d(&lp), permission_engine::Decision::Allow, "granted");
    let mut deny = Policy::full();
    deny.categories
        .insert(PermissionCategory::Desktop, Rule::new(Level::Deny));
    lp.set_global_policy(deny);
    assert_eq!(
        d(&lp),
        permission_engine::Decision::Deny,
        "a deny stays ahead of the grant"
    );
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn control_grant_is_scoped_to_the_computer_it_was_given_on() {
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "web_extract")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let fetch = Arc::new(with_class(
        "web_extract",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "x",
    ));
    register(&lp, &[&ty]);
    lp.sh.tools.register(Arc::clone(&fetch) as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    let _pause = next_card(&mut rx).await;
    let ctx = lp.sh.runs.lock().get(&run).cloned().unwrap();
    let target = ok(ControlKind::Type).unwrap();
    let grants = ctx.control_grants.lock().clone();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].computer, format!("host:{MACHINE_A}"));
    assert!(ctx.control_covers("desktop_type", PermissionCategory::Desktop, &target));
    // The same program on another computer is not covered.
    *ctx.control_grants.lock() = vec![crate::shared::ControlGrant {
        computer: format!("host:{MACHINE_B}"),
        identity: target.identity.clone(),
    }];
    assert!(!ctx.control_covers("desktop_type", PermissionCategory::Desktop, &target));
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn control_grant_dies_with_the_run_and_is_not_kept_for_the_next_turn() {
    let env = setup();
    let mut replies = vec![
        calls(&[("a0", "fs_read", json!({}))]),
        calls(&[("a1", "desktop_type", json!({"text": "x"}))]),
        text("first done"),
        calls(&[("b0", "fs_read", json!({}))]),
        calls(&[("b1", "desktop_type", json!({"text": "x"}))]),
        text("second done"),
    ];
    replies.shrink_to_fit();
    let fake = Fake::new(replies);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    register(&lp, &[&ty]);
    let mut rx = lp.subscribe();
    let run1 = lp.run_turn(env.chat, "one").unwrap();
    let c1 = drive(&lp, &env, &mut rx, run1, |_| ApprovalScope::RunControl).await;
    assert_eq!(c1.len(), 1);
    assert!(
        lp.sh.runs.lock().get(&run1).is_none(),
        "the run's context is gone"
    );
    let run2 = lp.run_turn(env.chat, "two").unwrap();
    let c2 = drive(&lp, &env, &mut rx, run2, |_| ApprovalScope::Once).await;
    assert_eq!(c2.len(), 1, "the next turn asks again");
    assert_eq!(ty.count.load(Ordering::SeqCst), 2);
}

// ----------------------------------------------- where the scope is refused

#[tokio::test]
async fn run_control_scope_is_refused_on_a_card_without_a_control_target() {
    // A shell card, an egress-only card, and an input card whose backend has
    // no process identity: none of them accepts the run-control scope.
    for (name, script) in [
        ("shell_run", vec![None]),
        ("script_run", vec![None]),
        ("desktop_type", vec![None]),
        ("desktop_launch", vec![None]),
    ] {
        let env = setup();
        let fake = script_type_then(&[("c1", name)]);
        let lp = control_loop(&env, &fake, false, true);
        let tool = ControlTool::new(name, script);
        register(&lp, &[&tool]);
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "go").unwrap();
        let card = next_card(&mut rx).await;
        assert_eq!(control_card(&card), Value::Null, "{name}");
        assert!(
            matches!(
                lp.approve(card.call_id, ApprovalScope::RunControl, env.user),
                Err(LoopError::OnceOnly(_))
            ),
            "{name}"
        );
        lp.approve(card.call_id, ApprovalScope::Once, env.user)
            .unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    }
    // An egress-only card (no executor, not even a desktop tool).
    let env = setup();
    let fake = script_type_then(&[("c1", "web_extract")]);
    let lp = control_loop(&env, &fake, false, true);
    let fetch = Arc::new(with_class(
        "web_extract",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "x",
    ));
    lp.sh.tools.register(Arc::clone(&fetch) as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let card = next_card(&mut rx).await;
    assert!(matches!(
        lp.approve(card.call_id, ApprovalScope::RunControl, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.approve(card.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
}

#[tokio::test]
async fn the_grant_is_not_offered_for_a_refused_program() {
    for (refusal, with_computer) in [
        (Some("terminal"), true),
        (Some("elevated"), true),
        (Some("browser"), true),
    ] {
        let env = setup();
        let fake = script_type_then(&[("c1", "desktop_type")]);
        let lp = control_loop(&env, &fake, false, with_computer);
        let ty = ControlTool::new("desktop_type", vec![target_for(ControlKind::Type, refusal)]);
        register(&lp, &[&ty]);
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "go").unwrap();
        let card = next_card(&mut rx).await;
        assert_ne!(control_card(&card)["offered"], true, "{refusal:?}");
        assert!(matches!(
            lp.approve(card.call_id, ApprovalScope::RunControl, env.user),
            Err(LoopError::OnceOnly(_))
        ));
        lp.approve(card.call_id, ApprovalScope::Once, env.user)
            .unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(ty.guards.lock().as_slice(), &[None]);
    }
}

#[tokio::test]
async fn no_process_identity_means_no_grant_offer() {
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "desktop_type")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![None]);
    register(&lp, &[&ty]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |_| ApprovalScope::Once).await;
    assert_eq!(cards.len(), 2, "every call asks, as in #89");
    assert!(cards.iter().all(|c| control_card(c) == Value::Null));
}

#[tokio::test]
async fn a_waived_bot_keeps_asking_for_every_executor_call_without_a_grant() {
    // The executor rule is unchanged for everything the grant does not cover.
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "desktop_key")]);
    let lp = control_loop(&env, &fake, true, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let key = ControlTool::new("desktop_key", vec![ok(ControlKind::Key)]);
    register(&lp, &[&ty, &key]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |_| ApprovalScope::Once).await;
    assert_eq!(cards.len(), 2);
}

// ------------------------------------------------ review round tests

/// A run that read, typed (and was granted), then parks on an egress card, so a
/// test can inspect and probe the live context.
async fn parked_with_grant() -> (
    AgentLoop,
    Env,
    protocol::RunId,
    Arc<crate::shared::RunCtx>,
    tokio::sync::broadcast::Receiver<crate::StreamItem>,
) {
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "web_extract")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let fetch = Arc::new(with_class(
        "web_extract",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "x",
    ));
    register(&lp, &[&ty]);
    lp.sh.tools.register(Arc::clone(&fetch) as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    let _parked = next_card(&mut rx).await;
    let ctx = lp.sh.runs.lock().get(&run).cloned().unwrap();
    (lp, env, run, ctx, rx)
}

#[tokio::test]
async fn launch_shell_script_and_other_kinds_are_never_covered_even_with_a_matching_target() {
    let (lp, _env, run, ctx, _rx) = parked_with_grant().await;
    let typed = ok(ControlKind::Type).unwrap();
    // The grant is real: the typing call it was given for is covered.
    assert!(ctx.control_covers("desktop_type", PermissionCategory::Desktop, &typed));
    // The same eligible target, offered under another tool name, is not.
    for (name, category) in [
        ("desktop_launch", PermissionCategory::Desktop),
        ("desktop_drag", PermissionCategory::Desktop),
        ("shell_run", PermissionCategory::Shell),
        ("script_run", PermissionCategory::Script),
    ] {
        assert!(
            !ctx.control_covers(name, category, &typed),
            "{name} must never be covered"
        );
        let input = json!({"command": "echo x", "code": "1", "executable": "x"});
        let effect = if name == "shell_run" {
            Effect::Destructive
        } else {
            Effect::Write
        };
        let d = lp
            .sh
            .decide_with_target(&ctx, name, effect, category, &input, Some(&typed));
        assert!(d.is_ask(), "{name} still asks: {d:?}");
    }
    // A click is not an executor and never covered either.
    assert!(!ctx.control_covers("desktop_click", PermissionCategory::Desktop, &typed));
    // A target of another input kind does not cover the call either.
    let mut key = typed.clone();
    key.kind = ControlKind::Key;
    assert!(!ctx.control_covers("desktop_type", PermissionCategory::Desktop, &key));
    let mut set = typed.clone();
    set.kind = ControlKind::SetValue;
    assert!(!ctx.control_covers("desktop_key", PermissionCategory::Desktop, &set));
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn ask_first_policy_is_waived_but_limits_and_floors_still_win_over_a_grant() {
    let (lp, _env, run, ctx, _rx) = parked_with_grant().await;
    let t = ok(ControlKind::Type).unwrap();
    let input = json!({"text": "a"});
    let decide = |lp: &AgentLoop| {
        lp.sh.decide_with_target(
            &ctx,
            "desktop_type",
            Effect::Write,
            PermissionCategory::Desktop,
            &input,
            Some(&t),
        )
    };
    // Plain ask-first for the Desktop category: the grant stands in for the ask.
    let mut ask = Policy::full();
    ask.categories
        .insert(PermissionCategory::Desktop, Rule::new(Level::Ask));
    lp.set_global_policy(ask);
    assert_eq!(decide(&lp), permission_engine::Decision::Allow);
    // Unevaluated limits are a floor: the grant does not waive them.
    let mut limited = Policy::full();
    limited.categories.insert(
        PermissionCategory::Desktop,
        Rule::new(Level::AllowWithLimits(permission_engine::Limits {
            max_per_hour: Some(1),
            max_amount_minor: None,
            path_patterns: vec![],
        })),
    );
    lp.set_global_policy(limited);
    assert!(decide(&lp).is_ask(), "limits still ask");
    // Deny wins.
    let mut deny = Policy::full();
    deny.categories
        .insert(PermissionCategory::Desktop, Rule::new(Level::Deny));
    lp.set_global_policy(deny);
    assert_eq!(decide(&lp), permission_engine::Decision::Deny);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn desktop_drag_is_never_covered_by_the_grant() {
    let env = setup();
    let fake = script_type_then(&[("c1", "desktop_type"), ("c2", "desktop_drag")]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    let drag = ControlTool::new("desktop_drag", vec![ok(ControlKind::Type)]);
    register(&lp, &[&ty, &drag]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    let second = next_card(&mut rx).await;
    assert_eq!(
        second.tool_name, "desktop_drag",
        "a drag asks despite the grant"
    );
    assert_eq!(
        control_card(&second),
        Value::Null,
        "and offers no task-wide option"
    );
    assert!(matches!(
        lp.approve(second.call_id, ApprovalScope::RunControl, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.approve(second.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(drag.guards.lock().as_slice(), &[None]);
}

#[tokio::test]
async fn a_guarded_failure_withdraws_the_grant_and_the_next_call_asks_again() {
    let env = setup();
    let fake = script_type_then(&[
        ("c1", "desktop_type"),
        ("c2", "desktop_type"),
        ("c3", "desktop_type"),
    ]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    ty.fail_call.store(2, Ordering::SeqCst);
    register(&lp, &[&ty]);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    // First call: approved with the grant, covered, succeeds.
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    // The second call is covered, but the backend guard stops it part-way.
    // The third call must ask again: the failure withdrew the grant.
    let third = next_card(&mut rx).await;
    assert_eq!(third.tool_name, "desktop_type");
    assert_eq!(
        control_card(&third)["offered"],
        true,
        "a fresh offer, not a silent pass"
    );
    lp.approve(third.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(ty.count.load(Ordering::SeqCst), 3);
}

/// A tool that replays a recorded workflow through the loop gate, as
/// `workflow_run` does: one `authorize_control` per step.
struct ReplayLike {
    spec: ToolSpec,
    gate: crate::approval::LoopGate,
    steps: Vec<(&'static str, Option<ControlTarget>)>,
    covered: AtomicUsize,
    /// Withdraw the run's grants after the steps, as replay does when a guarded send fails.
    revoke: bool,
}

#[async_trait]
impl Tool for ReplayLike {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        use builtin_tools::{GateRequest, ToolGate};
        for (tool, control) in &self.steps {
            let req = GateRequest {
                run_id: ctx.run_id.clone(),
                tool: (*tool).to_owned(),
                category: PermissionCategory::Desktop,
                effect: Effect::Write,
                raw_command: String::new(),
                input: json!({"index": 1, "text": "a", "keys": "enter"}),
                content: builtin_tools::classify(tool).unwrap(),
                control: control.clone(),
            };
            let guard = self
                .gate
                .authorize_control(&req)
                .await
                .map_err(ToolError::Execution)?;
            if guard.is_some() {
                self.covered.fetch_add(1, Ordering::SeqCst);
            }
        }
        if self.revoke {
            use builtin_tools::ToolGate;
            self.gate.revoke_control(&ctx.run_id);
        }
        Ok(ToolOutput::text("replayed"))
    }
}

#[tokio::test]
async fn a_thirty_step_replay_costs_one_card_per_launch_grant_and_excluded_step() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c0", "fs_read", json!({}))]),
        calls(&[("c1", "replay_like", json!({}))]),
        text("done"),
    ]);
    let lp = control_loop(&env, &fake, false, true);
    // launch, 26 typing/key steps into one program, then 3 steps in a Save dialog.
    let mut steps: Vec<(&'static str, Option<ControlTarget>)> = vec![("desktop_launch", None)];
    for i in 0..26 {
        let (tool, kind) = if i % 2 == 0 {
            ("desktop_type", ControlKind::Type)
        } else {
            ("desktop_key", ControlKind::Key)
        };
        steps.push((tool, target_for(kind, None)));
    }
    steps.push((
        "desktop_type",
        target_for(ControlKind::Type, Some("dialog")),
    ));
    steps.push((
        "desktop_type",
        target_for(ControlKind::Type, Some("dialog")),
    ));
    steps.push(("desktop_key", target_for(ControlKind::Key, Some("dialog"))));
    assert_eq!(steps.len(), 30);
    let tool = Arc::new(ReplayLike {
        spec: ToolSpec {
            name: "replay_like".into(),
            description: "replay".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        gate: crate::approval::LoopGate {
            sh: Arc::downgrade(&lp.sh),
        },
        steps,
        covered: AtomicUsize::new(0),
        revoke: false,
    });
    lp.sh.tools.register(Arc::clone(&tool) as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let cards = drive(&lp, &env, &mut rx, run, |a| {
        if control_card(a)["offered"] == true {
            ApprovalScope::RunControl
        } else {
            ApprovalScope::Once
        }
    })
    .await;
    // launch + the one grant + three dialog steps: measured, there is no 1-3 bound.
    assert_eq!(cards.len(), 5);
    assert_eq!(
        tool.covered.load(Ordering::SeqCst),
        26,
        "all 26 steps ran under the one grant"
    );
}

#[tokio::test]
async fn a_guarded_failure_inside_a_replay_withdraws_the_grant_for_the_next_call() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c0", "fs_read", json!({}))]),
        calls(&[(
            "c1",
            "desktop_type",
            json!({"index": 1, "gen": 1, "text": "a"}),
        )]),
        calls(&[("c2", "replay_like", json!({}))]),
        calls(&[(
            "c3",
            "desktop_type",
            json!({"index": 1, "gen": 1, "text": "a"}),
        )]),
        text("done"),
    ]);
    let lp = control_loop(&env, &fake, false, true);
    let ty = ControlTool::new("desktop_type", vec![ok(ControlKind::Type)]);
    register(&lp, &[&ty]);
    let replay = Arc::new(ReplayLike {
        spec: ToolSpec {
            name: "replay_like".into(),
            description: "replay".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        gate: crate::approval::LoopGate {
            sh: Arc::downgrade(&lp.sh),
        },
        steps: vec![],
        covered: AtomicUsize::new(0),
        revoke: true,
    });
    lp.sh.tools.register(replay as Arc<dyn Tool>);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let first = next_card(&mut rx).await;
    lp.approve(first.call_id, ApprovalScope::RunControl, env.user)
        .unwrap();
    // The replay failed a guarded send and withdrew the grant: this asks again.
    let again = next_card(&mut rx).await;
    assert_eq!(control_card(&again)["offered"], true);
    lp.approve(again.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
}
