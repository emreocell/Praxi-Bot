//! Who or what let a call run (#697): every route that takes the place of an approval card
//! journals a `tool_call_approved` that says so (`granted_by`, the level and, for a remembered
//! choice, the rule), and a plain policy allow says nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use permission_engine::profiles::NewRule;
use permission_engine::{Capability, Policy, ProfileAction, SubjectKind};
use protocol::event::{AutonomyLevel, GrantedBy, ToolCallApproved};
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::full_lane_tests::{Asks, Start, ctx_of, decide, start_run};
use crate::tests::{
    Fake, calls, cfg, lane_cfg, make_loop, profile, setup, t, text, tool, until, with_class,
};
use crate::{ApprovalScope, StreamItem};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
const BALANCED: &str = "[permissions]\npreset = \"approve-destructive\"\n";
const ASK: &str = "[permissions]\npreset = \"read-and-draft\"\n";

/// The approval events the loop has sent so far, in order.
fn approvals(rx: &mut tokio::sync::broadcast::Receiver<StreamItem>) -> Vec<ToolCallApproved> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        if let Event::ToolCallApproved(a) = item.envelope.event {
            out.push(a);
        }
    }
    out
}

/// What an event says, as `(granted_by, level, rule_id)`.
fn provenance(a: &ToolCallApproved) -> (Option<GrantedBy>, Option<AutonomyLevel>, Option<&str>) {
    (a.granted_by, a.level, a.rule_id.as_deref())
}

/// A bot on the Asking level: one call of `tool` per script entry, the first answered on its
/// card with `scope`. Returns the approval events of the run.
async fn answer_first_card(scope: ApprovalScope, calls_made: usize) -> Vec<ToolCallApproved> {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let mut script: Vec<_> = (0..calls_made)
        .map(|i| calls(&[(&format!("c{i}"), "t.write", json!({"n": i}))]))
        .collect();
    script.push(text("done"));
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(script),
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(card.call_id, scope, env.user).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), calls_made);
    let events = approvals(&mut rx);
    assert!(events.iter().all(|a| a.approved_by == env.user));
    events
}

#[tokio::test]
async fn an_owner_card_answer_says_so() {
    let events = answer_first_card(ApprovalScope::Once, 1).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        provenance(&events[0]),
        (Some(GrantedBy::OwnerCard), Some(AutonomyLevel::Ask), None)
    );
}

/// "Allow for this chat", "always" and "always for this kind" each cover the next call, and the
/// event names the rule that did.
#[tokio::test]
async fn a_remembered_choice_names_its_rule() {
    for (scope, rule) in [
        (ApprovalScope::ThisChat, "chat:t.write"),
        (ApprovalScope::Always, "always:t.write"),
        (ApprovalScope::AlwaysCategory, "category:fs"),
    ] {
        let events = answer_first_card(scope, 2).await;
        assert_eq!(events.len(), 2, "{scope:?}");
        assert_eq!(
            provenance(&events[0]),
            (Some(GrantedBy::OwnerCard), Some(AutonomyLevel::Ask), None),
            "{scope:?}"
        );
        assert_eq!(
            provenance(&events[1]),
            (
                Some(GrantedBy::RememberedChoice),
                Some(AutonomyLevel::Ask),
                Some(rule)
            ),
            "{scope:?}"
        );
    }
}

/// A rule that is remembered per target (a command prefix) names the tool, never the command.
#[tokio::test]
async fn a_scoped_choice_never_carries_the_command_or_path() {
    let env = setup();
    let shell = Arc::new(with_class(
        "shell_run",
        Effect::Write,
        PermissionCategory::Shell,
        ContentClass::none(),
        "ok",
    ));
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![
            calls(&[("c1", "shell_run", json!({"command": "git status"}))]),
            calls(&[("c2", "shell_run", json!({"command": "git status -s"}))]),
            text("done"),
        ]),
        &[t(&shell)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(card.call_id, ApprovalScope::ThisChat, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let events = approvals(&mut rx);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].rule_id.as_deref(), Some("chat:shell_run"));
    let json = serde_json::to_string(&events).unwrap();
    assert!(!json.contains("git"), "{json}");
}

#[test]
fn a_rule_id_is_a_scope_and_a_plain_name_or_just_the_scope() {
    use crate::approval::rule_id;
    assert_eq!(rule_id("always", "shell_run"), "always:shell_run");
    assert_eq!(rule_id("chat", "t.write"), "chat:t.write");
    // An MCP server chooses its tool names: anything that is not a plain identifier is dropped.
    for odd in [
        "mcp:srv/tool",
        "a b",
        "C:/Users/me/secret.txt",
        "",
        &"x".repeat(65),
    ] {
        assert_eq!(rule_id("always", odd), "always", "{odd:?}");
    }
}

/// Which run starts get which Full-level provenance: the owner at the keyboard, and a schedule
/// or trigger the owner set up. Someone else's run gets no waiver, so no automatic grant.
#[tokio::test]
async fn the_full_lane_says_whose_run_it_served() {
    for (start, expected) in [
        (Start::Owner, GrantedBy::FullAccess),
        (Start::Schedule, GrantedBy::OwnerSchedule),
        (Start::Trigger, GrantedBy::OwnerSchedule),
    ] {
        let env = setup();
        let mail = Asks::new(
            "email_send",
            PermissionCategory::Messaging,
            ContentClass::none(),
        );
        let lp = make_loop(
            &env.storage,
            env.bot,
            &Fake::new(vec![
                calls(&[("m", "email_send", json!({"to": ["a@example.net"]}))]),
                text("done"),
            ]),
            &[],
            profile(FULL),
            lane_cfg(Policy::full()),
        );
        lp.tool_registry()
            .register(Arc::clone(&mail) as Arc<dyn tool_registry::Tool>);
        let mut rx = lp.subscribe();
        let run = start_run(&lp, &env, start, "go");
        assert_eq!(
            lp.wait(run).await.unwrap(),
            RunState::Succeeded,
            "{start:?}"
        );
        let events = approvals(&mut rx);
        assert_eq!(events.len(), 1, "{start:?}: {events:?}");
        assert_eq!(
            provenance(&events[0]),
            (Some(expected), Some(AutonomyLevel::Full), None),
            "{start:?}"
        );
        assert_eq!(events[0].approved_by, env.user, "{start:?}");
        // The journal on disk has the same event, so a reopened run shows it too.
        let journal = storage::read_event_log(env.storage.data_dir(), run).unwrap();
        assert!(
            journal.iter().any(|e| matches!(
                &e.event,
                Event::ToolCallApproved(a) if a.granted_by == Some(expected)
            )),
            "{start:?}: not journaled"
        );
    }
}

#[tokio::test]
async fn a_run_the_lane_does_not_serve_gets_a_card_and_no_automatic_grant() {
    let env = setup();
    let mail = Asks::new(
        "email_send",
        PermissionCategory::Messaging,
        ContentClass::none(),
    );
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![calls(&[(
            "m",
            "email_send",
            json!({"to": ["a@example.net"]}),
        )])]),
        &[],
        profile(FULL),
        lane_cfg(Policy::full()),
    );
    lp.tool_registry()
        .register(Arc::clone(&mail) as Arc<dyn tool_registry::Tool>);
    let mut rx = lp.subscribe();
    let run = start_run(&lp, &env, Start::Channel, "go");
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    lp.cancel(run).unwrap();
    let events = approvals(&mut rx);
    assert_eq!(events.len(), 0, "{events:?}");
}

/// Money asks at every level. Answered on its card at Full, it is an owner card, not the lane.
#[tokio::test]
async fn money_on_full_is_an_owner_card_not_the_lane() {
    let env = setup();
    let pay = Asks::new("ask_money", PermissionCategory::Money, ContentClass::none());
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![
            calls(&[("p", "ask_money", json!({"amount": 5}))]),
            text("done"),
        ]),
        &[],
        profile(FULL),
        lane_cfg(Policy::full()),
    );
    lp.tool_registry()
        .register(Arc::clone(&pay) as Arc<dyn tool_registry::Tool>);
    let mut rx = lp.subscribe();
    let run = start_run(&lp, &env, Start::Owner, "go");
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(card.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let events = approvals(&mut rx);
    assert_eq!(events.len(), 1);
    assert_eq!(
        provenance(&events[0]),
        (Some(GrantedBy::OwnerCard), Some(AutonomyLevel::Full), None)
    );
}

#[tokio::test]
async fn the_balanced_level_says_so() {
    let env = setup();
    let write = tool("fs_write", Effect::Write, "w");
    let write = Arc::new(write);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![
            calls(&[("c1", "fs_write", json!({"path": "a.txt"}))]),
            text("done"),
        ]),
        &[t(&write)],
        profile(BALANCED),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let events = approvals(&mut rx);
    assert_eq!(events.len(), 1);
    assert_eq!(
        provenance(&events[0]),
        (
            Some(GrantedBy::BalancedLevel),
            Some(AutonomyLevel::Balanced),
            None
        )
    );
}

/// What a plain allow (a read the level lets through, a category the policy allows) asks no
/// question about, so it has no event: the chain keeps folding those steps together.
#[tokio::test]
async fn a_plain_policy_allow_has_no_approval_event() {
    for (extra, config, effect) in [
        (ASK, cfg(Policy::approve_destructive()), Effect::Read),
        (FULL, cfg(Policy::full()), Effect::Write),
        (FULL, lane_cfg(Policy::full()), Effect::Write),
    ] {
        let env = setup();
        let w = Arc::new(tool("t.any", effect, "w"));
        let lp = make_loop(
            &env.storage,
            env.bot,
            &Fake::new(vec![calls(&[("c1", "t.any", json!({}))]), text("done")]),
            &[t(&w)],
            profile(extra),
            config,
        );
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "go").unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(w.count.load(Ordering::SeqCst), 1);
        assert!(approvals(&mut rx).is_empty(), "{extra}");
    }
}

/// The owner's own per-site rule answers the card for that site.
#[tokio::test]
async fn an_owner_site_rule_says_so() {
    let env = setup();
    let click = Arc::new(with_class(
        "browser_click",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::none(),
        "ok",
    ));
    *click.page_origin.lock() = Some("https://example.com:443".into());
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![
            calls(&[("c1", "browser_click", json!({"ref": "e1"}))]),
            text("done"),
        ]),
        &[t(&click)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    lp.add_profile_rule(
        env.bot,
        &NewRule {
            kind: SubjectKind::Site,
            raw_key: "example.com",
            include_subdomains: false,
            capability: Capability::Interact,
            action: ProfileAction::Allow,
            expires_at_ms: None,
        },
    )
    .unwrap();
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let events = approvals(&mut rx);
    assert_eq!(events.len(), 1);
    assert_eq!(
        provenance(&events[0]),
        (Some(GrantedBy::SiteRule), Some(AutonomyLevel::Ask), None)
    );
}

/// A secret-fill tool whose owner-created standing grant takes the card's place.
struct GrantedFill {
    spec: tool_registry::ToolSpec,
    count: AtomicUsize,
    grant_id: &'static str,
}

#[async_trait::async_trait]
impl tool_registry::Tool for GrantedFill {
    fn spec(&self) -> &tool_registry::ToolSpec {
        &self.spec
    }
    fn always_ask_for(&self, _input: &Value) -> bool {
        true
    }
    async fn standing_grant(
        &self,
        _ctx: &tool_registry::ToolContext,
        _input: &Value,
    ) -> Option<tool_registry::StandingGrant> {
        Some(tool_registry::StandingGrant {
            id: self.grant_id.into(),
            label: "secret CANARY-SECRET-NAME on https://canary.example.test".into(),
        })
    }
    async fn call(
        &self,
        _ctx: &tool_registry::ToolContext,
        _input: Value,
    ) -> Result<tool_registry::ToolOutput, tool_registry::ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(tool_registry::ToolOutput::text("typed"))
    }
}

#[tokio::test]
async fn a_standing_grant_says_so_by_id_and_never_by_its_label() {
    for (grant_id, rule) in [("g1", "standing:g1"), ("not a plain id/../x", "standing")] {
        let env = setup();
        let fill = Arc::new(GrantedFill {
            spec: tool_registry::ToolSpec {
                name: "browser_fill".into(),
                description: "fill".into(),
                input_schema: json!({"type": "object"}),
                effect: Effect::Write,
                category: PermissionCategory::Browser,
                platforms: vec![protocol::Platform::All],
                content: builtin_tools::classify("browser_fill").unwrap(),
            },
            count: AtomicUsize::new(0),
            grant_id,
        });
        let lp = make_loop(
            &env.storage,
            env.bot,
            &Fake::new(vec![
                calls(&[("c1", "browser_fill", json!({"secret": "x"}))]),
                text("done"),
            ]),
            &[],
            profile(""),
            cfg(Policy::full()),
        );
        lp.tool_registry()
            .register(Arc::clone(&fill) as Arc<dyn tool_registry::Tool>);
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "go").unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(fill.count.load(Ordering::SeqCst), 1);
        let events = approvals(&mut rx);
        assert_eq!(events.len(), 1, "{grant_id}");
        assert_eq!(
            provenance(&events[0]),
            (
                Some(GrantedBy::StandingGrant),
                Some(AutonomyLevel::Full),
                Some(rule)
            ),
            "{grant_id}"
        );
        let json = serde_json::to_string(&events).unwrap();
        assert!(
            !json.contains("CANARY") && !json.contains("canary"),
            "{json}"
        );
    }
}

/// A run whose workflow replay read untrusted content asks even at the Full level, and a call
/// that asks has no automatic grant to journal: the provenance goes away with the allow.
#[tokio::test]
async fn a_tainted_run_loses_the_allow_and_its_provenance() {
    let mail = Asks::new(
        "email_send",
        PermissionCategory::Messaging,
        ContentClass::none(),
    );
    let (_env, lp, ctx) = ctx_of(
        Start::Owner,
        FULL,
        lane_cfg(Policy::full()),
        std::slice::from_ref(&mail),
    )
    .await;
    let decide_now = || {
        lp.sh.decide_with_grant(
            &ctx,
            &mail.spec.name,
            mail.spec.effect,
            mail.spec.category,
            &json!({}),
            None,
        )
    };
    let clean = decide_now();
    assert!(!clean.decision.is_ask());
    let grant = clean
        .grant
        .expect("the lane's allow carries its provenance");
    assert_eq!(grant.granted_by, GrantedBy::FullAccess);
    assert_eq!(grant.level, AutonomyLevel::Full);
    // The decision the plain tests see is unchanged by the provenance.
    assert_eq!(
        decide(&lp, &ctx, &mail, &json!({})).1,
        Some("mode.full.policy")
    );

    lp.sh.router.tainted_runs.lock().insert(ctx.run_id);
    let tainted = decide_now();
    assert!(tainted.decision.is_ask());
    assert_eq!(tainted.reason, None);
    assert_eq!(tainted.grant, None);
}

/// The same for an allow a rule decided inside the pipeline (the Balanced lane): once the run
/// is tainted the allow becomes a card, and no provenance is left over for the journal.
#[tokio::test]
async fn a_tainted_run_loses_a_lane_grant_too() {
    let env = setup();
    let write = Arc::new(tool("fs_write", Effect::Write, "w"));
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![text("ok")]),
        &[t(&write)],
        profile(BALANCED),
        cfg(Policy::approve_destructive()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    lp.wait(run).await.unwrap();
    let row = env.storage.get_run(run).unwrap().unwrap();
    lp.sh.runs.lock().clear();
    let ctx = lp.sh.ctx(&row).unwrap();
    let decide_now = || {
        lp.sh.decide_with_grant(
            &ctx,
            "fs_write",
            Effect::Write,
            PermissionCategory::Fs,
            &json!({"path": "a.txt"}),
            None,
        )
    };
    let clean = decide_now();
    assert!(!clean.decision.is_ask());
    assert_eq!(
        clean.grant.map(|g| g.granted_by),
        Some(GrantedBy::BalancedLevel)
    );
    lp.sh.router.tainted_runs.lock().insert(ctx.run_id);
    let tainted = decide_now();
    assert!(tainted.decision.is_ask());
    assert_eq!(tainted.grant, None);
}
