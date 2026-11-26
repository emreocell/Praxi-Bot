//! `ToolGate::authorize_unattended`: the answer a `tool_self_check` probe needs. It is the
//! loop's own decision with every way of asking removed: a tool the bot cannot see, policy
//! denies, policy would ask about (or a run that read untrusted content would be asked about)
//! is refused, and no approval card is ever opened.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate};
use parking_lot::Mutex;
use permission_engine::{Level, Policy, Rule};
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::tests::{calls, cfg, make_loop, profile, setup, text, with_class};

type Answers = Vec<(&'static str, Result<(), String>)>;

/// Asks the loop's gate, from inside a run, whether each named tool may run unattended.
struct GateProbe {
    spec: ToolSpec,
    gate: crate::approval::LoopGate,
    names: Vec<&'static str>,
    rounds: Mutex<Vec<Answers>>,
}

#[async_trait]
impl Tool for GateProbe {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let registry = self
            .gate
            .sh
            .upgrade()
            .expect("the loop is alive")
            .tools
            .clone();
        let mut answers = Vec::new();
        for name in &self.names {
            // What the real caller (the self-check) sends: the registry's own facts.
            let spec = registry.get(name).map(|t| t.spec().clone());
            let req = GateRequest {
                run_id: ctx.run_id.clone(),
                tool: (*name).to_owned(),
                category: spec
                    .as_ref()
                    .map_or(PermissionCategory::Meta, |s| s.category),
                effect: spec.as_ref().map_or(Effect::Read, |s| s.effect),
                raw_command: String::new(),
                input: json!({}),
                content: spec.map_or_else(ContentClass::none, |s| s.content),
                control: None,
            };
            answers.push((*name, self.gate.authorize_unattended(&req).await));
        }
        self.rounds.lock().push(answers);
        Ok(ToolOutput::text("probed"))
    }
}

fn answer<'a>(round: &'a Answers, name: &str) -> &'a Result<(), String> {
    &round.iter().find(|(n, _)| *n == name).expect("probed").1
}

#[tokio::test]
async fn a_probe_runs_only_what_the_bot_may_call_without_asking_and_never_opens_a_card() {
    let env = setup();
    let fake = crate::tests::Fake::new(vec![
        calls(&[("p1", "gate_probe", json!({}))]),
        // The run now holds a stranger's text.
        calls(&[("r1", "page_read", json!({}))]),
        calls(&[("p2", "gate_probe", json!({}))]),
        text("done"),
    ]);
    // Everything allowed, except: accounts ask, files are denied.
    let mut policy = Policy::full();
    policy
        .categories
        .insert(PermissionCategory::Accounts, Rule::new(Level::Ask));
    policy
        .categories
        .insert(PermissionCategory::Fs, Rule::new(Level::Deny));
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("[tools]\nexclude = [\"t_hidden\"]\n"),
        cfg(policy),
    );
    let register = |name: &str, effect, category, class| {
        lp.sh
            .tools
            .register(Arc::new(with_class(name, effect, category, class, "x")) as Arc<dyn Tool>);
    };
    register(
        "t_ok",
        Effect::Read,
        PermissionCategory::Meta,
        ContentClass::none(),
    );
    // A write in an asking category really asks; a read there is allowed by the loop's read
    // shortcut, for the probe exactly as for the bot itself.
    register(
        "t_ask",
        Effect::Write,
        PermissionCategory::Accounts,
        ContentClass::none(),
    );
    register(
        "t_read_shortcut",
        Effect::Read,
        PermissionCategory::Accounts,
        ContentClass::none(),
    );
    register(
        "t_deny",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::none(),
    );
    register(
        "t_hidden",
        Effect::Read,
        PermissionCategory::Meta,
        ContentClass::none(),
    );
    // A mailbox read: allowed by policy, but it reaches a remote service.
    register(
        "t_egress",
        Effect::Read,
        PermissionCategory::Messaging,
        ContentClass::egress(),
    );
    register(
        "page_read",
        Effect::Read,
        PermissionCategory::Meta,
        ContentClass::untrusted(),
    );
    let mut other_bot = with_class(
        "t_other_bots",
        Effect::Read,
        PermissionCategory::Meta,
        ContentClass::none(),
        "x",
    );
    other_bot.bots = Some(vec!["someone-else".into()]);
    lp.sh.tools.register(Arc::new(other_bot) as Arc<dyn Tool>);
    let probe = Arc::new(GateProbe {
        spec: ToolSpec {
            name: "gate_probe".into(),
            description: "asks the gate".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Meta,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        gate: crate::approval::LoopGate {
            sh: Arc::downgrade(&lp.sh),
        },
        names: vec![
            "t_ok",
            "t_ask",
            "t_read_shortcut",
            "t_deny",
            "t_hidden",
            "t_other_bots",
            "t_egress",
            "t_missing",
        ],
        rounds: Mutex::new(Vec::new()),
    });
    lp.sh.tools.register(Arc::clone(&probe) as Arc<dyn Tool>);

    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "probe").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let rounds = probe.rounds.lock().clone();
    assert_eq!(rounds.len(), 2);

    // A clean run: allowed tools pass; asking, denied and hidden ones are refused.
    let clean = &rounds[0];
    assert!(answer(clean, "t_ok").is_ok(), "{clean:?}");
    assert!(answer(clean, "t_egress").is_ok(), "{clean:?}");
    assert!(answer(clean, "t_read_shortcut").is_ok(), "{clean:?}");
    let ask = answer(clean, "t_ask").as_ref().unwrap_err();
    assert!(ask.contains("needs approval"), "{ask}");
    let deny = answer(clean, "t_deny").as_ref().unwrap_err();
    assert!(deny.contains("not allowed"), "{deny}");
    for hidden in ["t_hidden", "t_other_bots", "t_missing"] {
        let e = answer(clean, hidden).as_ref().unwrap_err();
        assert!(e.contains("unknown tool"), "{hidden}: {e}");
    }

    // After the run read a stranger's text, a call that reaches a remote service would ask:
    // the probe is refused, the harmless one still passes.
    let tainted = &rounds[1];
    assert!(answer(tainted, "t_ok").is_ok(), "{tainted:?}");
    let e = answer(tainted, "t_egress").as_ref().unwrap_err();
    assert!(e.contains("needs approval"), "{e}");

    // And not one approval card was opened for any of it.
    while let Ok(item) = rx.try_recv() {
        assert!(
            !matches!(item.envelope.event, Event::ApprovalNeeded(_)),
            "a probe must never ask the owner"
        );
    }
}
