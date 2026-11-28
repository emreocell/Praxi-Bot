//! The outgoing-mail guardrails through the real `email_send` tool (`AgentMail` mock server).

use std::sync::Arc;

use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::agentmail::AgentMailEndpoint;
use crate::{AccountConfig, AccountKind, EmailConnector, tools};

const BOT: &str = "11111111-1111-4111-8111-111111111111";
const INBOX: &str = "bot@agentmail.to";

async fn setup() -> (MockServer, Arc<EmailConnector>) {
    setup_with(&[("work", INBOX)]).await
}

/// One `AgentMail` mock server and one account per `(name, inbox)`, all bound to [`BOT`].
async fn setup_with(accounts: &[(&str, &str)]) -> (MockServer, Arc<EmailConnector>) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let mut list = Vec::new();
    for (name, inbox) in accounts {
        let inbox_path = inbox.replace('@', "%40");
        Mock::given(method("POST"))
            .and(path(format!("/v0/inboxes/{inbox_path}/messages/send")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"message_id":"sent1","thread_id":"t"})),
            )
            .mount(&server)
            .await;
        let acct = AccountConfig {
            name: (*name).into(),
            address: (*inbox).into(),
            username: String::new(),
            password: SecretRef::new(format!("email/{name}/api_key")).unwrap(),
            imap: crate::ImapSettings::default(),
            smtp: crate::SmtpSettings::default(),
            drafts_folder: "Drafts".into(),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: Some(BOT.into()),
            allowed_senders: Vec::new(),
            kind: AccountKind::AgentMail,
            inbox_id: Some((*inbox).into()),
            identity: crate::IdentitySettings::default(),
        };
        store
            .set(acct.password.name(), "am_guard_test_KEY_77aa")
            .unwrap();
        list.push(acct);
    }
    let c = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap(),
    );
    c.replace_accounts(list).unwrap();
    (server, Arc::new(c))
}

fn send_tool(conn: &Arc<EmailConnector>) -> Arc<dyn Tool> {
    tools(conn)
        .into_iter()
        .find(|t| t.spec().name == "email_send")
        .unwrap()
}

fn ctx() -> ToolContext {
    ToolContext {
        bot_id: Some(BOT.into()),
        run_id: "run-1".into(),
        ..ToolContext::default()
    }
}

fn full_ctx() -> ToolContext {
    ToolContext {
        bot_id: Some(BOT.into()),
        run_id: "run-1".into(),
        unattended_mail: true,
        ..ToolContext::default()
    }
}

fn mail(to: &str) -> Value {
    json!({"to":[to],"subject":"Hello","body":"Offer text"})
}

async fn sent_bodies(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            let v: Value = serde_json::from_slice(&r.body).unwrap();
            v["text"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_clean_send_gets_the_opt_out_line_in_code_and_is_logged() {
    let (server, conn) = setup().await;
    let t = send_tool(&conn);
    assert!(t.always_ask(), "every send needs the owner's card");
    t.call(&ctx(), mail("bob@example.com")).await.unwrap();
    let bodies = sent_bodies(&server).await;
    assert!(bodies[0].starts_with("Offer text"), "{}", bodies[0]);
    assert!(bodies[0].contains("-- \n"), "{}", bodies[0]);
    assert!(bodies[0].contains("STOP"), "{}", bodies[0]);
    assert!(
        !bodies[0].contains("UNSUBSCRIBE") && !bodies[0].contains("will not write to you again"),
        "the line must not promise an automatic stop: {}",
        bodies[0]
    );
    assert!(bodies[0].contains(INBOX), "sender is named in the line");
    let log = conn.guard().log(10);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].recipients, ["bob@example.com"]);
    assert_eq!(log[0].message_id, "sent1");
    assert_eq!(log[0].bot.as_deref(), Some(BOT));
    assert!(log[0].opt_out_line && log[0].overridden.is_empty());
}

#[tokio::test]
async fn a_model_cannot_send_to_an_opted_out_address_without_the_owner_card() {
    let (server, conn) = setup().await;
    conn.guard()
        .add_opt_out("ann@example.com", "asked")
        .unwrap();
    let t = send_tool(&conn);
    // No card was shown: refused, nothing goes out, whatever the arguments say.
    let e = t.call(&ctx(), mail("Ann@Example.com")).await.unwrap_err();
    let ToolError::Execution(m) = e else {
        panic!("{e:?}")
    };
    assert!(m.contains("Only the owner"), "{m}");
    // Invented override arguments are not accepted at all.
    for field in ["force", "send_anyway", "override", "approved"] {
        let mut input = mail("ann@example.com");
        input[field] = json!(true);
        assert!(matches!(
            t.call(&ctx(), input).await,
            Err(ToolError::InvalidInput(_))
        ));
    }
    // Dropping the opt-out line is only a request and creates a warning of its own.
    let mut input = mail("bob@example.com");
    input["opt_out_line"] = json!(false);
    assert!(t.call(&ctx(), input).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_owner_can_send_anyway_once_and_it_is_logged() {
    let (server, conn) = setup().await;
    conn.guard().add_opt_out("@example.com", "").unwrap();
    let t = send_tool(&conn);
    let input = mail("ann@example.com");
    // The card shows the warning, the legal sentence and the exact text with the opt-out line.
    let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
    assert_eq!(d["send_anyway"], true);
    assert_eq!(d["warnings"][0]["code"], "opt_out_listed");
    assert!(d["legal_notice"].as_str().unwrap().contains("6563"));
    assert!(d["body"].as_str().unwrap().contains("STOP"));
    // Owner clicked "Send anyway": the call matching that card goes through, once.
    t.call(&ctx(), input.clone()).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(conn.guard().log(1)[0].overridden, ["opt_out_listed"]);
    assert!(t.call(&ctx(), input.clone()).await.is_err(), "one-shot");
    // A different text than the card showed does not ride on the approval.
    t.approval_details(&ctx(), &input).await.unwrap();
    let mut changed = input.clone();
    changed["body"] = json!("something else");
    assert!(t.call(&ctx(), changed).await.is_err());
    // An approval that ended without a send is dropped.
    let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
    t.approval_closed(&ctx(), &input, d["approval_ref"].as_str());
    assert!(t.call(&ctx(), input).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_daily_count_warns_but_the_owner_may_still_send() {
    let (_server, conn) = setup().await;
    let mut s = conn.guard().settings();
    s.daily_limit = 1;
    s.same_recipient_days = 0;
    conn.guard().set_settings(s).unwrap();
    let t = send_tool(&conn);
    t.call(&ctx(), mail("a@example.com")).await.unwrap();
    let second = mail("b@example.com");
    let d = t.approval_details(&ctx(), &second).await.unwrap().unwrap();
    assert_eq!(d["warnings"][0]["code"], "daily_limit");
    assert!(
        d["legal_notice"].is_null(),
        "legal note only for opt-out rights"
    );
    t.call(&ctx(), second).await.unwrap();
    assert_eq!(conn.guard().log(10).len(), 2);
}

/// The warning codes the owner's card would show for a mail to `to`.
async fn card_codes(t: &Arc<dyn Tool>, to: Value) -> Vec<String> {
    let input = json!({"to": to, "subject":"Hello","body":"Offer text"});
    let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
    d["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn spelling_variants_of_an_opted_out_mailbox_still_warn() {
    let (_server, conn) = setup().await;
    conn.guard().add_opt_out("ann@example.com", "").unwrap();
    conn.guard().add_opt_out("Bücher.example", "").unwrap();
    let t = send_tool(&conn);
    for to in [
        "ANN@EXAMPLE.COM",
        "ann+promo@example.com",
        "\"Ann\" <Ann+x@Example.com>",
        "Ann <ann@example.com>",
        "ann@xn--bcher-kva.example",
        "ann@Bücher.example",
        "ann@shop.xn--bcher-kva.example",
    ] {
        assert_eq!(
            card_codes(&t, json!([to])).await,
            ["opt_out_listed"],
            "{to}"
        );
    }
    // A trailing dot is the same host for DNS; if the address parser refuses it, nothing is sent.
    if let Ok(codes) = {
        let input = json!({"to":["ann@example.com."],"subject":"s","body":"b"});
        t.approval_details(&ctx(), &input).await
    } {
        assert_eq!(codes.unwrap()["warnings"][0]["code"], "opt_out_listed");
    }
    assert_eq!(
        card_codes(&t, json!(["bob@example.com"])).await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn the_same_recipient_wait_and_opt_out_by_unicode_entry_match_the_other_spelling() {
    let (_server, conn) = setup().await;
    conn.guard().add_opt_out("ann@münchen.de", "").unwrap();
    let t = send_tool(&conn);
    assert_eq!(
        card_codes(&t, json!(["ann@xn--mnchen-3ya.de"])).await,
        ["opt_out_listed"]
    );
    let (_s2, conn2) = setup().await;
    let mut settings = conn2.guard().settings();
    settings.same_recipient_days = 14;
    conn2.guard().set_settings(settings).unwrap();
    let t2 = send_tool(&conn2);
    t2.call(&ctx(), mail("tom@example.com")).await.unwrap();
    // `tom+2@` is the same mailbox: the same-recipient wait applies.
    assert_eq!(
        card_codes(&t2, json!(["Tom+2@Example.com"])).await,
        ["same_recipient"]
    );
}

#[tokio::test]
async fn the_guard_runs_again_for_the_identity_chosen_on_the_card() {
    const ALT: &str = "alt@agentmail.to";
    let (server, conn) = setup_with(&[("work", INBOX), ("alt", ALT)]).await;
    let mut s = conn.guard().settings();
    s.daily_limit = 1;
    s.same_recipient_days = 0;
    conn.guard().set_settings(s).unwrap();
    conn.guard()
        .add_opt_out("ann@example.com", "asked")
        .unwrap();
    let t = send_tool(&conn);
    let with = |account: &str, to: &str| json!({"account": account, "to": [to], "subject": "Hello", "body": "Offer text"});
    // One mail from "work" uses up its daily count.
    t.call(&ctx(), with("work", "a@example.com")).await.unwrap();
    // The card for "work" warns about the count; the card for "alt" (a fresh identity) does not.
    let work = with("work", "b@example.com");
    let d = t.approval_details(&ctx(), &work).await.unwrap().unwrap();
    assert_eq!(d["account"], "work");
    assert_eq!(d["warnings"][0]["code"], "daily_limit");
    let alt = with("alt", "b@example.com");
    let d = t.approval_details(&ctx(), &alt).await.unwrap().unwrap();
    assert_eq!(d["account"], "alt");
    assert_eq!(d["from"], ALT);
    assert!(d["warnings"].as_array().unwrap().is_empty(), "{d}");
    // The opt-out list applies whichever address is chosen (switching From is no way around it).
    for who in ["work", "alt"] {
        let input = with(who, "ann@example.com");
        let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
        assert_eq!(d["warnings"][0]["code"], "opt_out_listed", "{who}");
        t.approval_closed(&ctx(), &input, d["approval_ref"].as_str());
    }
    // The owner's "send anyway" for the card of "work" does not carry over to "alt" or back.
    t.approval_details(&ctx(), &work).await.unwrap();
    let mut other = work.clone();
    other["account"] = json!("alt");
    other["to"] = json!(["ann@example.com"]);
    assert!(
        t.call(&ctx(), other).await.is_err(),
        "no approval for this identity"
    );
    t.call(&ctx(), work).await.unwrap();
    assert_eq!(conn.guard().log(10)[0].account, "work");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn full_level_sends_past_repeat_recipient_and_daily_warnings() {
    let (server, conn) = setup().await;
    let mut s = conn.guard().settings();
    s.daily_limit = 1;
    s.same_recipient_days = 7;
    conn.guard().set_settings(s).unwrap();
    let t = send_tool(&conn);
    let who = "tom@example.com";
    t.call(&full_ctx(), mail(who)).await.unwrap();
    let second = mail(who);
    let out = t.call(&full_ctx(), second).await.unwrap();
    let ContentPart::Json { data: body } = &out.content[0] else {
        panic!("json output");
    };
    let warned = body
        .get("mail_guard_warnings")
        .and_then(Value::as_array)
        .map_or(0, std::vec::Vec::len);
    assert_eq!(warned, 2, "{body:?}");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(conn.guard().log(10).len(), 2);
}

#[tokio::test]
async fn full_level_still_blocks_the_opt_out_list() {
    let (server, conn) = setup().await;
    conn.guard().add_opt_out("ann@example.com", "").unwrap();
    let t = send_tool(&conn);
    let err = t
        .call(&full_ctx(), mail("ann@example.com"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::PermissionDenied(ref m) if m.contains("(opt-out list)")),
        "{err}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn full_level_cannot_remove_the_owner_enabled_footer() {
    let (server, conn) = setup().await;
    let mut input = mail("bob@example.com");
    input["opt_out_line"] = json!(false);
    let err = send_tool(&conn).call(&full_ctx(), input).await.unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn full_level_blocks_unreadable_guard_with_repair_instructions() {
    let (server, conn) = setup().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("guard-settings.json"), "{bad json").unwrap();
    conn.set_guard_dir(dir.path().to_path_buf());
    let err = send_tool(&conn)
        .call(&full_ctx(), mail("bob@example.com"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::PermissionDenied(ref m) if m.contains("Repair") && !m.contains("Remove the recipient")),
        "{err}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn literal_backslash_n_in_the_body_becomes_real_line_breaks_on_the_card_and_in_the_mail() {
    let (server, conn) = setup().await;
    let t = send_tool(&conn);
    let input = json!({
        "to": ["bob@example.com"],
        "subject": "Hello\\nthere",
        "body": "Merhaba,\\n\\nBen Praxi.\\r\\nIyi gunler\\tdilerim!"
    });
    let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
    let card = d["body"].as_str().unwrap();
    assert!(
        card.starts_with("Merhaba,\n\nBen Praxi.\r\nIyi gunler\tdilerim!"),
        "{card:?}"
    );
    assert!(!card.contains("\\n"), "{card:?}");
    assert_eq!(d["subject"], "Hello there");
    t.call(&ctx(), input).await.unwrap();
    let sent = sent_bodies(&server).await;
    assert!(
        sent[0].starts_with("Merhaba,\n\nBen Praxi.\r\n"),
        "{:?}",
        sent[0]
    );
    assert!(!sent[0].contains("\\n"), "{:?}", sent[0]);
}

#[tokio::test]
async fn a_body_that_already_has_real_line_breaks_is_never_altered() {
    let (server, conn) = setup().await;
    let t = send_tool(&conn);
    let body = "Use the \\n escape in C.\nSecond line";
    let input = json!({"to": ["bob@example.com"], "subject": "Code", "body": body});
    let d = t.approval_details(&ctx(), &input).await.unwrap().unwrap();
    assert!(d["body"].as_str().unwrap().starts_with(body));
    t.call(&ctx(), input).await.unwrap();
    assert!(sent_bodies(&server).await[0].starts_with(body));
}

#[tokio::test]
async fn the_thread_id_and_the_mission_of_a_send_are_logged() {
    let (_server, conn) = setup().await;
    conn.set_mission_resolver(Arc::new(|run: &str| {
        (run == "run-1").then(|| "mission-7".to_owned())
    }));
    let t = send_tool(&conn);
    t.call(&ctx(), mail("bob@example.com")).await.unwrap();
    let e = conn.guard().log(1).remove(0);
    assert_eq!(e.thread_id.as_deref(), Some("t"));
    assert_eq!(e.mission_id.as_deref(), Some("mission-7"));
    // A reply that quotes the id, or comes in the thread, finds it.
    let by_id = conn.guard().find_sent("work", "sent1", None).unwrap();
    assert_eq!(by_id.mission_id.as_deref(), Some("mission-7"));
    assert!(conn.guard().find_sent("work", "x", Some("t")).is_some());
}

fn tight_budget(conn: &Arc<EmailConnector>, budget: u32) {
    let mut s = conn.guard().settings();
    s.outreach_daily_budget = budget;
    s.outreach_warmup_days = 0;
    s.daily_limit = 0;
    s.same_recipient_days = 0;
    conn.guard().set_settings(s).unwrap();
}

#[tokio::test]
async fn an_unattended_bot_is_refused_past_the_daily_outreach_budget() {
    let (server, conn) = setup().await;
    tight_budget(&conn, 2);
    let t = send_tool(&conn);
    t.call(&full_ctx(), mail("a@example.com")).await.unwrap();
    t.call(&full_ctx(), mail("b@example.com")).await.unwrap();
    let e = t
        .call(&full_ctx(), mail("c@example.com"))
        .await
        .unwrap_err();
    let ToolError::Execution(m) = e else {
        panic!("{e:?}")
    };
    assert!(m.contains("Not sent") && m.contains("2 of 2"), "{m}");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(conn.guard().log(10).len(), 2);
    // The owner at the screen decides for their own mail: the budget is for outreach.
    t.call(&ctx(), mail("c@example.com")).await.unwrap();
    // A mission session is outreach whoever approved the card.
    conn.set_mission_resolver(Arc::new(|_| Some("m".to_owned())));
    assert!(t.call(&ctx(), mail("d@example.com")).await.is_err());
    // Raising the budget lets it through again.
    tight_budget(&conn, 10);
    t.call(&ctx(), mail("d@example.com")).await.unwrap();
}

#[tokio::test]
async fn a_mail_the_owner_asked_for_in_chat_is_never_held_to_the_outreach_budget() {
    let (server, conn) = setup().await;
    tight_budget(&conn, 1);
    let t = send_tool(&conn);
    // The owner's own chat turn on a Full bot: every call carries `unattended_mail`, but the
    // mail is the owner's request, so the budget does not count it out.
    let owner_chat = ToolContext {
        owner_chat_turn: true,
        ..full_ctx()
    };
    for to in ["a@example.com", "b@example.com", "c@example.com"] {
        t.call(&owner_chat, mail(to)).await.unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    // The same Full bot running in the background is still held: its one slot is used up.
    let e = t
        .call(&full_ctx(), mail("d@example.com"))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::Execution(m) if m.contains("Not sent")));
    // A mission session counts against the budget even if a context said "owner chat".
    conn.set_mission_resolver(Arc::new(|_| Some("m".to_owned())));
    assert!(t.call(&owner_chat, mail("e@example.com")).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn parallel_unattended_sends_cannot_share_the_last_slot_of_the_budget() {
    let (server, conn) = setup().await;
    tight_budget(&conn, 3);
    let mut tasks = Vec::new();
    for i in 0..8 {
        let t = send_tool(&conn);
        tasks.push(tokio::spawn(async move {
            t.call(&full_ctx(), mail(&format!("p{i}@example.com")))
                .await
                .is_ok()
        }));
    }
    let mut ok = 0;
    for t in tasks {
        ok += usize::from(t.await.unwrap());
    }
    assert_eq!(ok, 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}
