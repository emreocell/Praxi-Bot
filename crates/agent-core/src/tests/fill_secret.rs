//! Loop-level guarantees of owner-stored secret references for the fill
//! tools: always ask (whatever the policy), no value in any sink, exact
//! standing grants, restart mid-approval, and the redactor.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use builtin_tools::browser::{BrowserDriver, FieldTarget, Wait};
use secret_store::{FillSecrets, MemoryStore, SecretStore};

use super::*;

mod vault_leak;

const VALUE: &str = "hunter2-Correct-Horse-9!";
const PLANTED: &str = "planted-control-string-7731";
const GITHUB: &str = "https://github.com";

/// A scripted browser: knows where each ref lives, remembers what reached the page.
#[derive(Default)]
struct Page {
    targets: Mutex<HashMap<String, FieldTarget>>,
    typed_secrets: Mutex<Vec<String>>,
    typed_text: Mutex<Vec<String>>,
    snapshot_text: Mutex<String>,
    echo: bool,
    url: Mutex<Option<String>>,
    snapshot_pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    /// A ref whose input handler throws an error that quotes the typed value.
    throw_on: Mutex<Option<String>>,
}

fn target(frame: &str) -> FieldTarget {
    FieldTarget {
        name: "Password".into(),
        role: "textbox".into(),
        frame_origin: Some(frame.into()),
        top_origin: Some(frame.into()),
        sensitive: false,
    }
}

#[async_trait]
impl BrowserDriver for Page {
    async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
        self.targets
            .lock()
            .get(r)
            .cloned()
            .ok_or_else(|| format!("unknown ref {r}"))
    }
    async fn fill_secret(
        &self,
        r: &str,
        text: &str,
        accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        let live = self
            .targets
            .lock()
            .get(r)
            .and_then(|t| t.frame_origin.clone());
        if !accept(live) {
            return Err("refused: nothing was typed".into());
        }
        if self.throw_on.lock().as_deref() == Some(r) {
            return Err(format!("input handler threw: {text}"));
        }
        self.typed_secrets.lock().push(text.to_owned());
        Ok(if self.echo {
            format!("filled {r}; field shows {text}")
        } else {
            format!("filled {r}")
        })
    }
    async fn open(&self, _: &str, _: Option<&str>) -> Result<String, String> {
        Ok("opened".into())
    }
    async fn current_url(&self) -> Option<String> {
        self.url.lock().clone()
    }
    async fn snapshot(&self, _: bool) -> Result<String, String> {
        let pause = self.snapshot_pause.lock().take();
        if let Some((entered, resume)) = pause {
            entered.notify_one();
            resume.notified().await;
        }
        Ok(self.snapshot_text.lock().clone())
    }
    async fn click(&self, _: &str) -> Result<String, String> {
        Ok("clicked".into())
    }
    async fn fill(&self, r: &str, t: &str) -> Result<String, String> {
        self.typed_text.lock().push(t.to_owned());
        Ok(format!("filled {r}"))
    }
    async fn select(&self, _: &str, _: &str) -> Result<String, String> {
        Ok("selected".into())
    }
    async fn press(&self, _: &str) -> Result<String, String> {
        Ok("pressed".into())
    }
    async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
        Ok("scrolled".into())
    }
    async fn wait(&self, _: Wait, _: Option<Duration>) -> Result<String, String> {
        Ok("waited".into())
    }
    async fn back(&self) -> Result<String, String> {
        Ok("back".into())
    }
    async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
        Ok("tabs".into())
    }
    async fn screenshot(
        &self,
        _: bool,
        _: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Ok((vec![0xFF, 0xD8, 0xFF], "jpg"))
    }
    async fn eval(&self, _: &str) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

struct Rig {
    env: Env,
    lp: AgentLoop,
    fake: Arc<Fake>,
    page: Arc<Page>,
    secrets: Arc<FillSecrets>,
    store: Arc<MemoryStore>,
    redactor: crate::SharedRedactor,
}

fn rig(policy: Policy, echo: bool, replies: Vec<Reply>) -> Rig {
    let env = setup();
    let store = Arc::new(MemoryStore::new());
    let redactor = crate::SharedRedactor::default();
    let hook = redactor.clone();
    let secrets = Arc::new(
        FillSecrets::in_memory(store.clone() as Arc<dyn SecretStore>)
            .with_value_hook(Arc::new(move |v: &str| hook.register(v))),
    );
    let mut config = cfg(policy);
    config.redactor = Some(redactor.clone());
    let fake = Fake::new(replies);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    secrets
        .upsert_with(
            "login/github",
            secret_store::FillSecretUpdate {
                value: Some(VALUE),
                origins: Some(&[GITHUB.to_owned()]),
                bot_ids: Some(&[env.bot.to_string()]),
                ..secret_store::FillSecretUpdate::default()
            },
        )
        .unwrap();
    let page = Arc::new(Page {
        echo,
        ..Page::default()
    });
    page.targets.lock().insert("e5".into(), target(GITHUB));
    for t in builtin_tools::browser::tools_with_secrets(
        page.clone(),
        &env.dir.path().join("bt"),
        None,
        Some(Arc::clone(&secrets)),
    ) {
        lp.sh.tools.register(t);
    }
    lp.sh
        .tools
        .register(builtin_tools::secrets_list_tool(Arc::clone(&secrets)));
    Rig {
        env,
        lp,
        fake,
        page,
        secrets,
        store,
        redactor,
    }
}

/// Every byte under `dir`, file by file.
fn files_containing(dir: &Path, needle: &str) -> Vec<String> {
    fn walk(dir: &Path, needle: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, needle, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let n = needle.as_bytes();
                if bytes.windows(n.len()).any(|w| w == n) {
                    out.push(p.display().to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, needle, &mut out);
    out
}

fn sinks_text(r: &Rig) -> String {
    // Events, approvals, audit, messages and steps, as the stores hold them.
    let mut s = String::new();
    for chat in [r.env.chat] {
        for m in r.env.storage.list_messages(chat).unwrap() {
            s.push_str(&m.content);
        }
    }
    let _ = write!(s, "{:?}", r.env.storage.list_audit(r.env.user).unwrap());
    for run in r.env.storage.list_runs(r.env.user).unwrap() {
        for st in r.env.storage.list_steps(run.id).unwrap() {
            s.push_str(&st.input.to_string());
            s.push_str(&st.output.map(|o| o.to_string()).unwrap_or_default());
        }
        for a in r.env.storage.list_approvals(run.id).unwrap() {
            s.push_str(&a.summary);
        }
    }
    s
}

fn fill_secret_call() -> Reply {
    calls(&[(
        "c1",
        "browser_fill",
        json!({"ref": "e5", "secret": "login/github"}),
    )])
}

/// Guard: `Tool::always_ask_for` in `Shared::must_ask`. A secret fill asks under
/// the FULL policy (and with read shortcuts on); the same fill with ordinary
/// text does not. Fails if `decide` stops consulting `always_ask_for`.
#[tokio::test]
async fn a_secret_fill_asks_even_under_the_full_policy_but_text_does_not() {
    let r = rig(
        Policy::full(),
        false,
        vec![fill_secret_call(), text("done")],
    );
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "log in").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "browser_fill");
    let d = a.details.clone().expect("the card carries the facts");
    assert_eq!(d["secret"], "login/github");
    assert_eq!(d["frame_origin"], GITHUB);
    assert!(!serde_json::to_string(&a).unwrap().contains(VALUE));
    assert!(
        r.page.typed_secrets.lock().is_empty(),
        "nothing typed before approval"
    );
    r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user)
        .unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(r.page.typed_secrets.lock().as_slice(), [VALUE]);

    // Control: the same tool with text under the same policy runs unasked.
    let r = rig(
        Policy::full(),
        false,
        vec![
            calls(&[("c1", "browser_fill", json!({"ref": "e5", "text": "hello"}))]),
            text("done"),
        ],
    );
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "type").unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    while let Ok(i) = rx.try_recv() {
        assert!(!matches!(i.envelope.event, Event::ApprovalNeeded(_)));
    }
    assert_eq!(r.page.typed_text.lock().as_slice(), ["hello"]);
}

/// Guard: approving with an "always" scope never turns a secret fill into an
/// unasked one (`must_ask` also forces `ApprovalScope::Once` in `commit`).
#[tokio::test]
async fn an_always_grant_never_waives_the_next_secret_fill() {
    let r = rig(
        Policy::approve_destructive(),
        false,
        vec![
            fill_secret_call(),
            calls(&[(
                "c2",
                "browser_fill",
                json!({"ref": "e5", "secret": "login/github"}),
            )]),
            text("done"),
        ],
    );
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "log in twice").unwrap();
    for _ in 0..2 {
        let Event::ApprovalNeeded(a) =
            until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
        else {
            unreachable!()
        };
        // An "always" answer is refused outright for this call (once only) ...
        assert!(
            matches!(
                r.lp.approve(a.call_id, ApprovalScope::Always, r.env.user),
                Err(crate::LoopError::OnceOnly(_))
            ),
            "a secret fill can only be approved once"
        );
        // ... and the card is still open, so a one-time yes works. The next
        // identical call then asks again.
        r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user)
            .unwrap();
    }
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(r.page.typed_secrets.lock().len(), 2);
}

/// Guard: the value is in no sink. The scan covers the databases, the event
/// log and the journal on disk, the stored steps / approvals / audit / messages,
/// and what the model was sent; a planted string in an ordinary fill proves the
/// scan can see those places.
#[tokio::test]
async fn the_value_is_absent_from_every_sink_with_a_positive_control() {
    let r = rig(
        Policy::full(),
        true, // the page echoes the typed value
        vec![
            calls(&[("p1", "browser_fill", json!({"ref": "e5", "text": PLANTED}))]),
            fill_secret_call(),
            text("done"),
        ],
    );
    let mut rx = r.lp.subscribe();
    // A second subscriber keeps the whole stream (the first one is consumed
    // while waiting for the card).
    let mut whole = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "log in").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user)
        .unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(r.page.typed_secrets.lock().as_slice(), [VALUE]);
    r.lp.shutdown(Duration::from_millis(300)).await;

    // Everything the model was sent.
    let mut sent = String::new();
    for i in 0..r.fake.n_requests() {
        sent.push_str(&serde_json::to_string(&r.fake.request(i).messages).unwrap());
    }
    // Events as streamed.
    let mut streamed = String::new();
    while let Ok(i) = whole.try_recv() {
        streamed.push_str(&serde_json::to_string(&i.envelope.event).unwrap());
    }
    let stored = sinks_text(&r);

    // Positive control: the planted string IS found in each place we scan.
    assert!(sent.contains(PLANTED), "control: model history");
    assert!(streamed.contains(PLANTED), "control: event stream");
    assert!(stored.contains(PLANTED), "control: stored steps");
    assert!(
        !files_containing(r.env.dir.path(), PLANTED).is_empty(),
        "control: the disk scan can see the planted string"
    );

    // The value itself, and its distinctive part, are nowhere.
    for needle in [VALUE, "Correct-Horse"] {
        assert!(!sent.contains(needle), "model history leaked {needle}");
        assert!(!streamed.contains(needle), "event stream leaked {needle}");
        assert!(!stored.contains(needle), "stores leaked {needle}");
        assert_eq!(
            files_containing(r.env.dir.path(), needle),
            Vec::<String>::new(),
            "files leaked {needle}"
        );
    }
    // The tool result the model read says what happened, without the value.
    let result = tool_messages(&r.fake.request(2)).join("\n");
    assert!(result.contains("filled e5"), "{result}");
    assert!(!result.contains("Correct-Horse"), "{result}");
}

/// Guard: a restart while the card is open leaves nothing on disk: the value is
/// not read until after the approval, so a parked / interrupted run never had
/// it. Control: the planted text of an ordinary parked fill IS on disk.
#[tokio::test]
async fn a_restart_mid_approval_leaves_no_value_on_disk() {
    let r = rig(Policy::full(), false, vec![fill_secret_call()]);
    let mut rx = r.lp.subscribe();
    let _run = r.lp.run_turn(r.env.chat, "log in").unwrap();
    let Event::ApprovalNeeded(_) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    // The card is open: nothing typed, value not read into any file.
    assert!(r.page.typed_secrets.lock().is_empty());
    let paused = r.lp.shutdown(Duration::from_millis(300)).await;
    assert!(!paused.is_empty(), "the run was paused mid-approval");
    let dir = r.env.dir.path().to_path_buf();
    assert_eq!(files_containing(&dir, VALUE), Vec::<String>::new());
    assert_eq!(
        files_containing(&dir, "Correct-Horse"),
        Vec::<String>::new()
    );
    // The reference (name) is what is stored.
    assert!(!files_containing(&dir, "login/github").is_empty());

    // Control: an ordinary parked fill leaves its text on disk, so the scan works.
    let r = rig(
        Policy::approve_destructive(),
        false,
        vec![calls(&[(
            "c1",
            "browser_fill",
            json!({"ref": "e5", "text": PLANTED}),
        )])],
    );
    let mut rx = r.lp.subscribe();
    let _ = r.lp.run_turn(r.env.chat, "type").unwrap();
    let _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    r.lp.shutdown(Duration::from_millis(300)).await;
    assert!(!files_containing(r.env.dir.path(), PLANTED).is_empty());
}

/// Guard: a field on a wrong origin is refused before any approval exists.
#[tokio::test]
async fn a_wrong_origin_never_produces_an_approvable_card() {
    let r = rig(
        Policy::full(),
        false,
        vec![fill_secret_call(), text("done")],
    );
    r.page
        .targets
        .lock()
        .insert("e5".into(), target("https://evil.test"));
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "log in").unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    while let Ok(i) = rx.try_recv() {
        assert!(
            !matches!(i.envelope.event, Event::ApprovalNeeded(_)),
            "no card may be offered for an origin the owner did not allow"
        );
    }
    assert!(r.page.typed_secrets.lock().is_empty());
    let result = tool_messages(&r.fake.request(1)).join("\n");
    assert!(
        result.contains("not allowed on https://evil.test"),
        "{result}"
    );
    assert!(r.env.storage.list_approvals(run).unwrap().is_empty());
}

/// Guard: a standing grant for exactly this secret + origin stands in for the
/// card, is audited by id, and does not extend to another origin.
#[tokio::test]
async fn an_exact_standing_grant_skips_the_card_and_is_audited() {
    let r = rig(
        Policy::full(),
        false,
        vec![
            fill_secret_call(),
            calls(&[(
                "c2",
                "browser_fill",
                json!({"ref": "e6", "secret": "login/github"}),
            )]),
            text("done"),
        ],
    );
    r.secrets
        .upsert(
            "login/github",
            None,
            Some(&[GITHUB.to_owned(), "https://gitlab.com".to_owned()]),
            None,
        )
        .unwrap();
    // e6 lives on the other allowed origin, which has no grant.
    r.page
        .targets
        .lock()
        .insert("e6".into(), target("https://gitlab.com"));
    let grant = r.secrets.grant_create("login/github", GITHUB).unwrap();
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "log in").unwrap();
    // The granted origin runs without a card; the other one asks.
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.input["ref"], "e6", "only the un-granted origin asks");
    assert_eq!(
        r.page.typed_secrets.lock().as_slice(),
        [VALUE],
        "e5 ran unasked"
    );
    r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user)
        .unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(r.page.typed_secrets.lock().len(), 2);
    let used: Vec<_> = r
        .env
        .storage
        .list_audit(r.env.user)
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "secret.standing_grant_used")
        .collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].detail["grant"], grant.id.as_str());
    assert!(!used[0].detail.to_string().contains(VALUE));
}

/// Defence in depth for the signed webhook / mail / external-schedule runs: even
/// with an exact standing grant row, an externally started run neither
/// consumes it (`standing_grant` stays `None`, so the card is asked) nor can it
/// create a new "Always" grant. A human run is the positive control.
#[tokio::test]
async fn an_external_run_never_uses_or_creates_a_standing_grant() {
    use crate::shared::GrantTarget;
    let r = rig(Policy::full(), false, vec![]);
    r.secrets.grant_create("login/github", GITHUB).unwrap();
    let ctx_for = |kind: String| {
        let run =
            r.lp.rt
                .create_run(r.env.user, r.env.bot, Some(r.env.chat), &kind)
                .unwrap();
        let row = r.env.storage.get_run(run).unwrap().unwrap();
        r.lp.sh.ctx(&row).unwrap()
    };
    let human = ctx_for("turn".into());
    let hook = ctx_for(format!("turn:incoming_webhook:{}:x", uuid::Uuid::now_v7()));
    let mail = ctx_for("turn:incoming_email:x".into());
    let ext = ctx_for(format!(
        "turn:external_schedule:{}",
        protocol::TaskId::new()
    ));
    assert_eq!(hook.initiator, protocol::InitiatorKind::ExternalSchedule);
    let input = json!({"ref": "e5", "secret": "login/github"});
    // Positive control: the same grant covers the human run.
    assert!(
        r.lp.sh
            .standing_grant(&human, "browser_fill", &input)
            .await
            .is_some()
    );
    for ctx in [&hook, &mail, &ext] {
        assert!(
            r.lp.sh
                .standing_grant(ctx, "browser_fill", &input)
                .await
                .is_none(),
            "an external run consumed a standing grant"
        );
    }
    // Creation: a human run records an Always grant, an external one cannot.
    let always = |name: &str| GrantTarget::Tool(name.into());
    let count = || r.lp.sh.grants.lock().always.len();
    let before = count();
    for ctx in [&hook, &mail, &ext] {
        r.lp.sh.add_grant(
            ctx,
            always("browser_fill"),
            tool_registry::PermissionCategory::Browser,
            "browser_fill",
        );
    }
    assert_eq!(count(), before, "an external run created a standing grant");
    r.lp.sh.add_grant(
        &human,
        always("browser_fill"),
        tool_registry::PermissionCategory::Browser,
        "browser_fill",
    );
    assert_eq!(count(), before + 1);
}

/// Guard: the production redactor masks a value a page echoes in tool output
/// (registered through the secrets service, and through a store refresh).
#[tokio::test]
async fn an_echoed_value_is_masked_by_the_production_redactor() {
    let r = rig(
        Policy::full(),
        false,
        vec![
            calls(&[("c1", "browser_snapshot", json!({}))]),
            text("done"),
        ],
    );
    *r.page.snapshot_text.lock() = format!("textbox \"Password\" value=\"{VALUE}\"");
    let run = r.lp.run_turn(r.env.chat, "look").unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    let result = tool_messages(&r.fake.request(1)).join("\n");
    assert!(result.contains("[REDACTED]"), "{result}");
    assert!(!result.contains("Correct-Horse"), "{result}");

    // A refresh from the store alone (a restart) masks it as well: the fill
    // namespace is part of "every value in the store".
    let fresh = crate::SharedRedactor::default();
    fresh.refresh_from_store(Some(&*r.store), &[]).unwrap();
    assert_eq!(fresh.redact(&format!("x {VALUE} y")), "x [REDACTED] y");
    assert!(!r.redactor.redact(VALUE).contains("Correct"));
}

/// Guard: bot B cannot fill or list a secret scoped only to bot A.
#[tokio::test]
async fn bot_b_cannot_fill_or_list_secret_scoped_to_bot_a() {
    let r = rig(
        Policy::full(),
        false,
        vec![
            calls(&[("c1", "secrets_list", json!({}))]),
            fill_secret_call(),
            text("done"),
        ],
    );
    // Re-scope the secret exclusively to a different bot id.
    let other_bot = BotId(uuid::Uuid::now_v7());
    r.secrets
        .upsert_with(
            "login/github",
            secret_store::FillSecretUpdate {
                bot_ids: Some(&[other_bot.to_string()]),
                ..secret_store::FillSecretUpdate::default()
            },
        )
        .unwrap();
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "use github secret").unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    while let Ok(i) = rx.try_recv() {
        assert!(
            !matches!(i.envelope.event, Event::ApprovalNeeded(_)),
            "no approval card may be offered for another bot's secret"
        );
    }
    assert!(r.page.typed_secrets.lock().is_empty());
    let listed = tool_messages(&r.fake.request(1)).join("\n");
    assert!(!listed.contains("login/github"), "{listed}");
    let fill_err = tool_messages(&r.fake.request(2)).join("\n");
    assert!(fill_err.contains("not enabled for this bot"), "{fill_err}");
}

/// Guard: the model can learn names and allowed origins, and only those.
#[tokio::test]
async fn the_model_can_list_secret_names_but_never_values() {
    let r = rig(
        Policy::full(),
        false,
        vec![calls(&[("c1", "secrets_list", json!({}))]), text("done")],
    );
    let run = r.lp.run_turn(r.env.chat, "what can you type").unwrap();
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Succeeded);
    let result = tool_messages(&r.fake.request(1)).join("\n");
    assert!(
        result.contains("login/github: https://github.com"),
        "{result}"
    );
    assert!(!result.contains(VALUE));
}

#[tokio::test]
async fn stop_after_handover_claim_cancels_inflight_typing_before_releasing_approval() {
    let r = rig(
        Policy::full(),
        false,
        vec![
            calls(&[("c1", "browser_handover", json!({"kind":"code"}))]),
            text("done"),
        ],
    );
    *r.page.url.lock() = Some(format!("{GITHUB}/2fa"));
    *r.page.snapshot_text.lock() = "# Verify\n  [e5] textbox \"Verification code\"".into();
    let mut rx = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "sign in").unwrap();
    let Event::HandoverRequested(card) =
        until(&mut rx, |e| matches!(e, Event::HandoverRequested(_))).await
    else {
        unreachable!()
    };
    assert!(r.lp.claim_handover(card.call_id).is_some());
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *r.page.snapshot_pause.lock() = Some((entered.clone(), resume.clone()));
    let typing = tokio::spawn(async move {
        builtin_tools::handover_hub()
            .type_code(&run.to_string(), "123456")
            .await
    });
    entered.notified().await;
    r.lp.cancel(run).unwrap();
    assert!(
        builtin_tools::handover_hub()
            .type_code(&run.to_string(), "123456")
            .await
            .is_err()
    );
    resume.notify_one();
    assert!(typing.await.unwrap().is_err());
    assert!(r.page.typed_secrets.lock().is_empty());
    assert_eq!(r.lp.wait(run).await.unwrap(), RunState::Cancelled);
    assert_eq!(r.fake.requests.lock().len(), 1);
}

/// Guard: the password the DAEMON generated for `account_register` is really typed
/// through `browser_fill {secret}` (the page echoes whatever it receives), and it
/// appears in no sink afterwards: model history, event stream, stored steps /
/// approvals / audit / messages, files on disk, logs, the account listing. The
/// planted string (typed as ordinary text) proves each scan can see its place.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one ordered scenario: run, then scan every sink
async fn a_generated_password_typed_through_browser_fill_is_in_no_sink() {
    let log = Arc::new(Mutex::new(String::new()));
    let _guard = tracing::subscriber::set_default(crate::account_register_tests::LogCapture(
        Arc::clone(&log),
    ));
    let r = rig(Policy::full(), true, vec![]);
    let bot = r.env.bot.to_string();
    let bot8: String = bot
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let name = format!("login/example.com-{bot8}");
    r.lp.sh.tools.register(builtin_tools::account_register_tool(
        Arc::clone(&r.secrets),
        builtin_tools::AccountRegisterConfig::default(),
    ));
    r.page
        .targets
        .lock()
        .insert("e7".into(), target("https://example.com"));
    r.fake.replies.lock().extend([
        calls(&[("p1", "browser_fill", json!({"ref": "e7", "text": PLANTED}))]),
        calls(&[(
            "a1",
            "account_register",
            json!({"site": "https://example.com", "login_id": "me@example.org"}),
        )]),
        calls(&[(
            "a2",
            "browser_fill",
            json!({"ref": "e7", "secret": name.clone()}),
        )]),
        text("done"),
    ]);

    let mut rx = r.lp.subscribe();
    let mut whole = r.lp.subscribe();
    let run = r.lp.run_turn(r.env.chat, "sign me up").unwrap();
    let wait = r.lp.wait(run);
    tokio::pin!(wait);
    let mut approvals = 0;
    let state = loop {
        tokio::select! {
            done = &mut wait => break done.unwrap(),
            Ok(i) = rx.recv() => {
                if let Event::ApprovalNeeded(a) = &i.envelope.event {
                    approvals += 1;
                    r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user).unwrap();
                }
            }
        }
    };
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(approvals, 2, "the sign-up and the typing each asked");
    r.lp.shutdown(Duration::from_millis(300)).await;

    // The password was generated, stored, and REALLY typed (the page received it).
    let account = r.secrets.accounts(None).unwrap().remove(0);
    assert_eq!(account.name, name);
    assert!(!account.unused, "typing marks the account used");
    let pw = r
        .secrets
        .resolve_value_for(&name, Some(&bot))
        .unwrap()
        .expose()
        .to_owned();
    assert!(pw.len() >= 20);
    assert_eq!(
        r.page.typed_secrets.lock().as_slice(),
        [pw.as_str()],
        "the page did not receive the generated password"
    );
    assert_eq!(r.page.typed_text.lock().as_slice(), [PLANTED]);

    let mut sent = String::new();
    for i in 0..r.fake.n_requests() {
        sent.push_str(&serde_json::to_string(&r.fake.request(i).messages).unwrap());
    }
    let mut streamed = String::new();
    while let Ok(i) = whole.try_recv() {
        streamed.push_str(&serde_json::to_string(&i.envelope.event).unwrap());
    }
    let stored = sinks_text(&r);
    let logs = log.lock().clone();
    let listing = format!("{:?}{:?}", r.secrets.accounts(None), r.secrets.list());

    // Controls: each scanned place does contain things from this run.
    assert!(
        sent.contains(PLANTED) && sent.contains(&name),
        "control: history"
    );
    assert!(streamed.contains(PLANTED), "control: event stream");
    assert!(stored.contains(PLANTED), "control: stored steps");
    assert!(
        !files_containing(r.env.dir.path(), PLANTED).is_empty(),
        "control: files"
    );
    assert!(listing.contains("example.com"), "control: listing");
    // The result of the typing, as the model read it, names the field and not the value.
    let last = r.fake.request(r.fake.n_requests() - 1);
    let results = tool_messages(&last).join("\n");
    assert!(results.contains("filled e7"), "{results}");

    for (place, haystack) in [
        ("model history", &sent),
        ("event stream", &streamed),
        ("stored rows", &stored),
        ("logs", &logs),
        ("listing", &listing),
        ("tool results", &results),
    ] {
        assert!(!haystack.contains(&pw), "{place} leaked the password");
    }
    assert_eq!(
        files_containing(r.env.dir.path(), &pw),
        Vec::<String>::new(),
        "files leaked the password"
    );
}

/// `account_register` makes the password; `browser_fill {secret}` then really types
/// it into a page that ECHOES what it was given. The scan covers the model's
/// requests, the events, every stored row, the files under the data directory and
/// the logs. Positive controls: the page received the password, the log capture
/// sees a planted line, and the echo text proves the result path was exercised.
#[tokio::test]
async fn an_account_password_typed_through_browser_fill_reaches_no_sink() {
    let log = Arc::new(Mutex::new(String::new()));
    let _guard = tracing::subscriber::set_default(crate::account_register_tests::LogCapture(
        Arc::clone(&log),
    ));
    tracing::info!(control = PLANTED, "log capture control line");
    let r = rig(Policy::full(), true, vec![]);
    r.lp.sh.tools.register(builtin_tools::account_register_tool(
        Arc::clone(&r.secrets),
        builtin_tools::AccountRegisterConfig::default(),
    ));
    let bot = r.env.bot.to_string();
    let short: String = bot
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let name = format!("login/example.com-{short}");
    r.page
        .targets
        .lock()
        .insert("e6".into(), target("https://example.com"));
    r.fake.replies.lock().extend([
        calls(&[(
            "c1",
            "account_register",
            json!({"site": "https://example.com", "login_id": "me@example.org"}),
        )]),
        calls(&[(
            "c2",
            "browser_fill",
            json!({"ref": "e6", "secret": name.clone()}),
        )]),
        text("signed up"),
    ]);
    let mut rx = r.lp.subscribe();
    let run =
        r.lp.run_turn(r.env.chat, "sign me up on example.com")
            .unwrap();
    let mut events = Vec::new();
    let mut approvals = 0;
    // The sign-up asks, then the typing asks: the owner approves each once.
    for _ in 0..2 {
        let ev = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
        let Event::ApprovalNeeded(a) = &ev else {
            unreachable!()
        };
        approvals += 1;
        events.push(format!("{ev:?}"));
        r.lp.approve(a.call_id, ApprovalScope::Once, r.env.user)
            .unwrap();
    }
    let state = r.lp.wait(run).await.unwrap();
    while let Ok(e) = rx.try_recv() {
        events.push(format!("{e:?}"));
    }
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(approvals, 2, "the sign-up and the typing each asked");

    let typed = r.page.typed_secrets.lock().clone();
    assert_eq!(typed.len(), 1, "the password was typed once");
    let pw = typed[0].clone();
    assert!(pw.len() >= 20);
    assert_eq!(
        r.secrets
            .resolve_value_for(&name, Some(&bot))
            .unwrap()
            .expose(),
        pw,
        "what the page got is the stored password"
    );
    assert!(!r.secrets.accounts(Some(&bot)).unwrap()[0].unused);

    let mut model_saw = String::new();
    for i in 0..r.fake.n_requests() {
        let _ = write!(model_saw, "{:?}", r.fake.request(i));
    }
    let haystack = [
        events.join("\n"),
        sinks_text(&r),
        model_saw,
        log.lock().clone(),
        format!("{:?}", r.secrets.list().unwrap()),
    ]
    .join("\n");
    assert!(haystack.contains(PLANTED), "the log scan is not vacuous");
    assert!(haystack.contains("example.com"), "the scan saw the run");
    assert!(!haystack.contains(&pw), "the typed password leaked");
    let data = r.env.dir.path();
    // The vault entry itself is the one place the value lives (the in-memory store
    // is not on disk); no file of the daemon may hold it.
    assert!(
        files_containing(data, &pw).is_empty(),
        "{:?}",
        files_containing(data, &pw)
    );
    assert!(r.page.typed_text.lock().is_empty());
    assert!(
        !files_containing(data, "example.com").is_empty(),
        "the file scan sees the run"
    );
}
