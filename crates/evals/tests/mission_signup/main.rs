//! End-to-end eval: an unattended Full mission signs up on four fake sites with its own mailbox.
//!
//! The owner types one request in a chat of a Full bot. From there everything is the real
//! daemon: `bot serve`'s HTTP API on a loopback port, the agent loop with the built-in tools
//! (browser, `account_register`, `email_wait_for`, TOTP capture, vault), the mission ledger, the
//! supervisor and its wake loop, the owner notices. Only the outside world is fake:
//!
//! * the model is scripted ([`model::SignupModel`]) and reads every result it acts on;
//! * four sites are real HTTP servers on loopback ([`sites`]): a plain sign-up, a sign-up that
//!   mails a confirmation link, a sign-up behind a CAPTCHA and one that makes the account set up
//!   two-factor authentication;
//! * the browser ([`browser::SiteBrowser`]) speaks HTTP to those servers under made-up public
//!   names and enforces the same origin checks the real driver reports;
//! * the bot's mailbox is a fake `AgentMail` server ([`mailbox`]) the real connector talks to.
//!
//! No network beyond loopback, no sleeping beyond the supervisor's few-millisecond polls, no keys.

mod browser;
mod checks;
mod mailbox;
mod model;
mod sites;
mod world;

use sites::{CAPTCHA, HOSTILE, PLAIN, TOTP_KEY, TWOFA, VERIFY, VERIFY_TOKEN};
use world::{MAIL_API_KEY, World};

const REQUEST: &str = "Register on these sites with your own email and keep a ledger of the results: \
    https://plain-one.example, https://verify-two.example, https://captcha-three.example, \
    https://twofa-four.example. Skip a site you cannot register on and tell me why in the report.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // one ordered walk: set up, run, then every assertion
async fn a_full_mission_registers_on_four_sites_with_its_own_mailbox_and_reports_once() {
    let w = World::start().await;

    // ---- the owner asks, in their own chat ----
    let sent = w.owner_says(REQUEST).await;
    assert_eq!(sent["delivery"], "new_turn", "{sent}");
    let mission = w.wait_for_mission().await;
    let id = mission["id"].as_str().unwrap().to_owned();
    assert_eq!(
        mission["goal"], REQUEST,
        "the goal is the owner's own words"
    );

    let end = w.wait_mission_end(&id).await;
    let items = w.items(&id);
    assert_eq!(
        end["mission"]["state"],
        "done",
        "mission: {end}\nitems: {items:#?}\nmodel calls: {}",
        w.model.model_calls()
    );

    // ---- the ledger: three signed up, one given up with its reason ----
    let status = |k: &str| {
        let i = items
            .iter()
            .find(|i| i.key == k)
            .unwrap_or_else(|| panic!("no item {k}"));
        (
            i.status.as_str().to_owned(),
            i.result.clone(),
            i.last_error.clone(),
        )
    };
    for site in [PLAIN, VERIFY, TWOFA] {
        let (s, result, _) = status(site);
        assert_eq!(s, "done", "{site}: {items:#?}");
        assert!(result.is_some_and(|r| !r.is_empty()), "{site} has evidence");
    }
    let (s, _, error) = status(CAPTCHA);
    assert_eq!(s, "failed", "{items:#?}");
    assert!(
        error.as_deref().is_some_and(|e| e.contains("CAPTCHA")),
        "the CAPTCHA site is given up with the reason: {error:?}"
    );
    assert_eq!(items.len(), 4);
    assert_eq!(end["mission"]["progress"]["done"], 3);
    assert_eq!(end["mission"]["progress"]["failed"], 1);
    assert_eq!(
        end["mission"]["progress"]["blocked"], 0,
        "nothing is left for the owner"
    );

    // ---- what the sites saw ----
    let book = w.sites.book.lock();
    let signed = |host: &str| book.signups.iter().filter(|s| s.0 == host).count();
    assert_eq!(signed(PLAIN), 1, "{:?}", book.hits);
    assert_eq!(signed(VERIFY), 1);
    assert_eq!(signed(TWOFA), 1);
    assert_eq!(signed(CAPTCHA), 0, "the CAPTCHA form was never submitted");
    assert_eq!(
        book.captcha_rejections, 0,
        "nobody tried to get past the check"
    );
    assert_eq!(
        book.verified,
        vec![w.mail.address().to_owned()],
        "the mailed link confirmed site two, with the bot's own address"
    );
    assert_eq!(
        book.tfa_enabled.len(),
        1,
        "site four got a correct TOTP code"
    );
    assert_eq!(book.tfa_wrong, 0);
    // Every sign-up used the bot's own address and a vault password, different per site.
    let mut passwords: Vec<String> = book.signups.iter().map(|s| s.2.clone()).collect();
    assert!(book.signups.iter().all(|s| s.1 == w.mail.address()));
    assert!(passwords.iter().all(|p| p.len() >= 16));
    passwords.sort();
    passwords.dedup();
    assert_eq!(passwords.len(), 3, "one password per site");
    // The captcha site was reached after the first two and before the last one (the mission
    // went on past it), and the confirmation link was opened after the mail arrived.
    let pos = |needle: &str| book.hits.iter().position(|h| h == needle).unwrap();
    assert!(pos(&format!("{CAPTCHA} GET /signup")) < pos(&format!("{TWOFA} POST /signup")));
    assert!(pos(&format!("{VERIFY} POST /signup")) < pos(&format!("{VERIFY} GET /confirm")));
    drop(book);

    // The bot never wandered off: no request to a host that is none of the four sites.
    assert!(
        w.browser.unreachable.lock().is_empty(),
        "the bot followed a link it was not given: {:?}",
        w.browser.unreachable.lock()
    );
    assert!(
        w.mail.writes().is_empty(),
        "a sign-up mission only reads its inbox: {:?}",
        w.mail.writes()
    );

    // ---- the vault: passwords and the TOTP key are kept for the bot, never shown ----
    w.assert_vault_holds_the_accounts();

    // ---- no card, no question, no hand-over anywhere ----
    w.assert_nobody_was_asked();

    // ---- the report: the owner is told exactly once ----
    let (_, detail) = w.api("GET", &format!("/v1/missions/{id}"), None).await;
    let mission_chat = detail["mission"]["chat_id"].as_str().unwrap().to_owned();
    w.assert_one_report(&id, &items).await;

    // ---- nothing leaks ----
    w.assert_no_leaks(
        &mission_chat,
        &[
            ("the site's hostile marker", HOSTILE),
            ("the evil host", "evil.example"),
            ("the confirmation token", VERIFY_TOKEN),
        ],
    )
    .await;
    let mut secrets: Vec<(String, String)> = w
        .sites
        .book
        .lock()
        .signups
        .iter()
        .map(|s| (format!("the password for {}", s.0), s.2.clone()))
        .collect();
    secrets.push(("the TOTP key".into(), TOTP_KEY.into()));
    secrets.push(("the mail provider's key".into(), MAIL_API_KEY.into()));
    w.assert_secrets_stay_secret(&id, &secrets).await;

    w.stop().await;
}
