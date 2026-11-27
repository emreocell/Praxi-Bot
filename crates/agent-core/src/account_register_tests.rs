//! `account_register`: how the egress / approval rules treat it, and that the
//! generated password reaches nothing the model, the owner's cards, the
//! database, the events or the logs can show.

use std::sync::Arc;

use permission_engine::Policy;
use protocol::{Event, InitiatorKind, PermissionCategory, TaskId};
use secret_store::{FillSecrets, MemoryStore};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::Effect;

use crate::tests::{Fake, calls, cfg, make_loop, profile, setup, text, until};

const INPUT: &str = r#"{"site":"https://example.com","login_id":"me@example.org"}"#;

fn secrets() -> Arc<FillSecrets> {
    Arc::new(FillSecrets::in_memory(Arc::new(MemoryStore::new())))
}

/// Captures every log line (message and fields) any thread of the test emits.
pub(crate) struct LogCapture(pub(crate) Arc<parking_lot::Mutex<String>>);

impl tracing::Subscriber for LogCapture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        attrs.record(&mut Grab(&self.0));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        values.record(&mut Grab(&self.0));
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut Grab(&self.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

struct Grab<'a>(&'a parking_lot::Mutex<String>);

impl tracing::field::Visit for Grab<'_> {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = writeln!(self.0.lock(), "{}={v:?}", f.name());
    }
}

#[test]
fn account_register_is_classified_as_egress_and_never_executes() {
    let class = builtin_tools::classify("account_register").unwrap();
    assert!(class.egress, "a sign-up after an untrusted read must ask");
    assert!(!class.executes);
    // Discarding an account is a local delete that always asks on its own.
    let discard = builtin_tools::classify("account_discard").unwrap();
    assert!(!discard.egress && !discard.ingests_untrusted && !discard.reads_private);
    // Keeping a two-factor key is a local write too: nothing leaves the machine.
    let totp = builtin_tools::classify("account_totp_save").unwrap();
    assert!(!totp.egress && !totp.ingests_untrusted && !totp.reads_private);
}

/// After an untrusted read, even with the egress waiver on and the Full preset,
/// and in a webhook run, the call asks (Allow is never the answer). A clean
/// run asks too: the tool declares `always_ask`.
#[tokio::test]
async fn account_register_asks_after_untrusted_reads_and_in_webhook_runs() {
    let env = setup();
    let lp = make_loop(
        &env.storage,
        env.bot,
        &Fake::new(vec![]),
        &[],
        profile("[permissions]\nallow_egress_after_reads = true\n"),
        cfg(Policy::full()),
    );
    lp.sh.tools.register(builtin_tools::account_register_tool(
        secrets(),
        builtin_tools::AccountRegisterConfig::default(),
    ));
    lp.sh
        .tools
        .register(builtin_tools::account_discard_tool(secrets()));
    lp.sh
        .tools
        .register(builtin_tools::account_totp_save_tool(secrets()));
    let ctx_for = |kind: String, untrusted: bool| {
        let run = lp
            .rt
            .create_run(env.user, env.bot, Some(env.chat), &kind)
            .unwrap();
        let row = env.storage.get_run(run).unwrap().unwrap();
        let ctx = lp.sh.ctx(&row).unwrap();
        ctx.taint.lock().exposure.untrusted = untrusted;
        ctx
    };
    let input: Value = serde_json::from_str(INPUT).unwrap();
    let hook = ctx_for(format!("turn:external_schedule:{}", TaskId::new()), false);
    assert_eq!(hook.initiator, InitiatorKind::ExternalSchedule);
    for (label, ctx) in [
        ("clean human run", ctx_for("turn".into(), false)),
        ("untrusted human run", ctx_for("turn".into(), true)),
        ("webhook run", hook),
    ] {
        let d = lp.sh.decide(
            &ctx,
            "account_register",
            Effect::Write,
            PermissionCategory::Meta,
            &input,
        );
        assert!(d.is_ask(), "{label}: {d:?}");
        let d = lp.sh.decide(
            &ctx,
            "account_discard",
            Effect::Write,
            PermissionCategory::Meta,
            &json!({"name": "login/x"}),
        );
        assert!(d.is_ask(), "discard, {label}: {d:?}");
        let d = lp.sh.decide(
            &ctx,
            "account_totp_save",
            Effect::Write,
            PermissionCategory::Meta,
            &json!({"name": "login/x", "setup": "JBSWY3DPEHPK3PXP"}),
        );
        assert!(d.is_ask(), "totp save, {label}: {d:?}");
    }
}

/// The password is generated and (here) typed by no one; whatever the run
/// produced is scanned for it. The scan also proves the sentinel is not
/// vacuous: the stored password is non-empty and differs per run.
#[tokio::test]
async fn the_generated_password_appears_in_no_result_event_row_or_log() {
    let log = Arc::new(parking_lot::Mutex::new(String::new()));
    let _guard = tracing::subscriber::set_default(LogCapture(Arc::clone(&log)));
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "account_register",
            serde_json::from_str(INPUT).unwrap(),
        )]),
        text("done"),
    ]);
    let secrets = secrets();
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    lp.sh.tools.register(builtin_tools::account_register_tool(
        Arc::clone(&secrets),
        builtin_tools::AccountRegisterConfig::default(),
    ));
    let mut rx = lp.subscribe();
    let mut events = Vec::new();
    let run = lp.run_turn(env.chat, "sign me up").unwrap();
    let ev = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let Event::ApprovalNeeded(card) = &ev else {
        unreachable!()
    };
    events.push(format!("{ev:?}"));
    lp.approve(card.call_id, crate::ApprovalScope::Once, env.user)
        .unwrap();
    // Drain every event the run emits until it ends.
    let state = loop {
        tokio::select! {
            done = lp.wait(run) => break done.unwrap(),
            Ok(e) = rx.recv() => events.push(format!("{e:?}")),
        }
    };
    while let Ok(e) = rx.try_recv() {
        events.push(format!("{e:?}"));
    }
    assert_eq!(state, RunState::Succeeded);

    let accounts = secrets.accounts(None).unwrap();
    assert_eq!(accounts.len(), 1, "the sign-up ran");
    assert!(accounts[0].unused, "nothing typed it yet");
    let bot = accounts[0].bot_ids[0].clone();
    let pw = secrets
        .resolve_value_for(&accounts[0].name, Some(&bot))
        .unwrap()
        .expose()
        .to_owned();
    assert!(pw.len() >= 20);

    let parts = [
        events.join(
            "
",
        ),
        format!("{:?}", env.storage.list_messages(env.chat).unwrap()),
        format!("{:?}", env.storage.list_steps(run).unwrap()),
        format!("{:?}", env.storage.list_approvals(run).unwrap()),
        format!("{:?}", env.storage.list_audit(env.user).unwrap()),
        format!(
            "{:?}",
            env.storage.list_audit_for_run(env.user, run).unwrap()
        ),
        log.lock().clone(),
        format!("{accounts:?}{:?}", secrets.list().unwrap()),
    ];
    let haystack = parts.join(
        "
",
    );
    assert!(!haystack.contains(&pw), "the password leaked");
    assert!(haystack.contains("example.com"), "the scan saw the run");
}
