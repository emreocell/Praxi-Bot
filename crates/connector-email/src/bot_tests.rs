//! Per-bot account binding, runtime reload and config validation.

use std::sync::Arc;

use secret_store::{MemoryStore, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError};

use crate::config::{normalize_sender, validate_account_name, validate_host};
use crate::testutil::{PASSWORD, Protocol, account, server};
use crate::{AccountConfig, EmailConnector, SmtpSecurity, tools};

const BOT_A: &str = "11111111-1111-4111-8111-111111111111";
const BOT_B: &str = "22222222-2222-4222-8222-222222222222";

fn store() -> Arc<MemoryStore> {
    let s = Arc::new(MemoryStore::new());
    s.set("email/test", PASSWORD).unwrap();
    s
}

fn named(base: &AccountConfig, name: &str, bot: Option<&str>) -> AccountConfig {
    let mut a = base.clone();
    a.name = name.into();
    a.bot = bot.map(str::to_owned);
    a
}

/// An account whose servers do not exist (nothing listens on port 1).
fn dead(base: &AccountConfig, name: &str, bot: Option<&str>) -> AccountConfig {
    let mut a = named(base, name, bot);
    a.imap.port = 1;
    a.smtp.port = 1;
    a
}

fn tool(conn: &Arc<EmailConnector>, name: &str) -> Arc<dyn Tool> {
    tools(conn)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

fn ctx(bot: Option<&str>) -> ToolContext {
    ToolContext {
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    }
}

async fn call(
    conn: &Arc<EmailConnector>,
    bot: Option<&str>,
    name: &str,
    input: Value,
) -> Result<Value, ToolError> {
    let out = tool(conn, name).call(&ctx(bot), input).await?;
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("expected JSON")
    };
    Ok(data.clone())
}

fn message_of(e: &ToolError) -> String {
    match e {
        ToolError::InvalidInput(m) | ToolError::Execution(m) => m.clone(),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn bot_a_cannot_use_bot_b_account_and_gets_the_same_error_as_for_a_missing_one() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn =
        Arc::new(EmailConnector::new(vec![named(&base, "b-mail", Some(BOT_B))], store()).unwrap());

    // The owner reaches it, by name and by default.
    let ok = call(
        &conn,
        Some(BOT_B),
        "email_list_folders",
        json!({"account":"b-mail"}),
    )
    .await
    .unwrap();
    assert_eq!(ok["folders"][0]["name"], "INBOX");
    let ok = call(&conn, Some(BOT_B), "email_list_folders", json!({}))
        .await
        .unwrap();
    assert_eq!(ok["folders"][0]["name"], "INBOX");
    let commands_before = imap.commands.lock().unwrap().len();

    // Another bot, a non-bot caller and a missing account all see the same error.
    let missing = call(
        &conn,
        Some(BOT_A),
        "email_list_folders",
        json!({"account":"nope"}),
    )
    .await
    .unwrap_err();
    let denied = call(
        &conn,
        Some(BOT_A),
        "email_list_folders",
        json!({"account":"b-mail"}),
    )
    .await
    .unwrap_err();
    let anonymous = call(
        &conn,
        None,
        "email_list_folders",
        json!({"account":"b-mail"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        message_of(&denied).replace("b-mail", "X"),
        message_of(&missing).replace("nope", "X")
    );
    assert_eq!(message_of(&anonymous), message_of(&denied));
    assert!(matches!(denied, ToolError::InvalidInput(_)));

    // Every tool is gated the same way, and nothing reached the server.
    for (name, input) in [
        ("email_search", json!({"account":"b-mail"})),
        ("email_read", json!({"account":"b-mail","uid":42})),
        ("email_mark_read", json!({"account":"b-mail","uid":42})),
        (
            "email_draft",
            json!({"account":"b-mail","to":["x@example.com"],"subject":"s","body":"b"}),
        ),
        (
            "email_send",
            json!({"account":"b-mail","to":["x@example.com"],"subject":"s","body":"b"}),
        ),
    ] {
        let e = call(&conn, Some(BOT_A), name, input).await.unwrap_err();
        assert!(message_of(&e).contains("unknown account"), "{name}: {e:?}");
    }
    assert_eq!(imap.commands.lock().unwrap().len(), commands_before);
    assert!(smtp.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_bot_defaults_to_its_own_bound_account_over_an_unbound_one() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    // "shared" is unbound and unreachable: success proves "mine" was used.
    let conn = Arc::new(
        EmailConnector::new(
            vec![
                dead(&base, "shared", None),
                named(&base, "mine", Some(BOT_A)),
                dead(&base, "theirs", Some(BOT_B)),
            ],
            store(),
        )
        .unwrap(),
    );
    assert_eq!(conn.scoped(Some(BOT_A)).resolve_name(None).unwrap(), "mine");
    let ok = call(&conn, Some(BOT_A), "email_list_folders", json!({}))
        .await
        .unwrap();
    assert_eq!(ok["folders"][0]["name"], "INBOX");
    // Bot B has its own account too; bot A's name does not leak to it.
    assert_eq!(
        conn.scoped(Some(BOT_B)).resolve_name(None).unwrap(),
        "theirs"
    );
    // A bot may still name an unbound account explicitly.
    assert_eq!(
        conn.scoped(Some(BOT_A))
            .resolve_name(Some("shared"))
            .unwrap(),
        "shared"
    );
}

#[tokio::test]
async fn unbound_accounts_behave_as_before() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn = Arc::new(EmailConnector::new(vec![base.clone()], store()).unwrap());
    for bot in [None, Some(BOT_A), Some(BOT_B)] {
        assert_eq!(conn.scoped(bot).resolve_name(None).unwrap(), "test");
        let ok = call(&conn, bot, "email_list_folders", json!({}))
            .await
            .unwrap();
        assert_eq!(ok["folders"][0]["name"], "INBOX");
    }
    // Two unbound accounts still need a name, as before.
    let two = EmailConnector::new(
        vec![named(&base, "one", None), named(&base, "two", None)],
        store(),
    )
    .unwrap();
    let err = two.scoped(Some(BOT_A)).resolve_name(None).unwrap_err();
    assert!(err.to_string().contains("several accounts configured"));
    assert!(err.to_string().contains("one, two"));
}

#[tokio::test]
async fn error_listings_name_only_accounts_the_bot_can_use() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn = EmailConnector::new(
        vec![
            named(&base, "one", None),
            named(&base, "two", None),
            named(&base, "secret-box", Some(BOT_B)),
        ],
        store(),
    )
    .unwrap();
    let text = conn
        .scoped(Some(BOT_A))
        .resolve_name(None)
        .unwrap_err()
        .to_string();
    assert!(text.contains("one, two"), "{text}");
    assert!(!text.contains("secret-box"), "{text}");
    // Only bound accounts of other bots exist: nothing is available.
    let only_other = EmailConnector::new(vec![named(&base, "b", Some(BOT_B))], store()).unwrap();
    let text = only_other
        .scoped(Some(BOT_A))
        .resolve_name(None)
        .unwrap_err()
        .to_string();
    assert!(text.contains("no e-mail account"), "{text}");
}

#[tokio::test]
async fn tools_are_hidden_from_a_bot_with_no_usable_account() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn =
        Arc::new(EmailConnector::new(vec![named(&base, "b-mail", Some(BOT_B))], store()).unwrap());
    let t = tool(&conn, "email_read");
    assert!(t.visible_to(Some(BOT_B)));
    assert!(!t.visible_to(Some(BOT_A)));
    assert!(!t.visible_to(None));
    // An unbound account makes the tools visible to every bot again.
    conn.replace_accounts(vec![named(&base, "open", None)])
        .unwrap();
    assert!(t.visible_to(Some(BOT_A)) && t.visible_to(None));
}

#[tokio::test]
async fn replacing_accounts_takes_effect_for_the_next_call_on_the_same_tools() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let base = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn = Arc::new(EmailConnector::reloadable(store()));
    let list = tool(&conn, "email_list_folders");
    assert!(!list.visible_to(Some(BOT_A)), "no accounts yet");
    let err = list.call(&ctx(Some(BOT_A)), json!({})).await.unwrap_err();
    assert!(message_of(&err).contains("no e-mail account"));

    conn.replace_accounts(vec![named(&base, "mine", Some(BOT_A))])
        .unwrap();
    assert!(list.visible_to(Some(BOT_A)));
    assert!(list.call(&ctx(Some(BOT_A)), json!({})).await.is_ok());

    // An invalid replacement is refused and the old set stays in force.
    let mut bad = named(&base, "bad", None);
    bad.imap.host = "evil host\r\n".into();
    assert!(conn.replace_accounts(vec![bad]).is_err());
    assert!(
        conn.replace_accounts(vec![base.clone(), base.clone()])
            .is_err()
    );
    assert_eq!(conn.account_names(), ["mine"]);
    assert!(list.call(&ctx(Some(BOT_A)), json!({})).await.is_ok());

    // Removing the account takes it away from the next call.
    conn.replace_accounts(Vec::new()).unwrap();
    assert!(list.call(&ctx(Some(BOT_A)), json!({})).await.is_err());
}

#[test]
fn new_fields_round_trip_and_old_files_still_load() {
    #[derive(serde::Deserialize, serde::Serialize)]
    struct File {
        #[serde(default, alias = "account")]
        accounts: Vec<AccountConfig>,
    }
    let old = r#"
[[accounts]]
name = "work"
address = "me@example.com"
password = "email/work/password"
[accounts.imap]
host = "imap.example.com"
[accounts.smtp]
host = "smtp.example.com"
"#;
    let f: File = toml::from_str(old).unwrap();
    assert_eq!(f.accounts[0].bot, None);
    assert_eq!(f.accounts[0].allowed_senders.len(), 0);
    f.accounts[0].validate().unwrap();
    // Unset new fields are not written back (old readers keep working on
    // files that never used them).
    let text = toml::to_string(&f).unwrap();
    assert!(
        !text.contains("bot") && !text.contains("allowed_senders"),
        "{text}"
    );

    let with_new = r#"
[[accounts]]
name = "work"
address = "me@example.com"
password = "email/work/password"
bot = "11111111-1111-4111-8111-111111111111"
allowed_senders = ["boss@example.com", "@corp.example"]
[accounts.imap]
host = "imap.example.com"
[accounts.smtp]
host = "smtp.example.com"
"#;
    let f: File = toml::from_str(with_new).unwrap();
    assert_eq!(f.accounts[0].bot.as_deref(), Some(BOT_A));
    assert_eq!(f.accounts[0].allowed_senders.len(), 2);
    let again: File = toml::from_str(&toml::to_string(&f).unwrap()).unwrap();
    assert_eq!(again.accounts, f.accounts);
}

fn valid() -> AccountConfig {
    AccountConfig {
        name: "work".into(),
        address: "me@example.com".into(),
        username: String::new(),
        password: secret_store::SecretRef::new("email/work/password").unwrap(),
        imap: crate::ImapSettings {
            host: "imap.example.com".into(),
            port: 993,
        },
        smtp: crate::SmtpSettings {
            host: "smtp.example.com".into(),
            port: 465,
            security: SmtpSecurity::Implicit,
        },
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot: None,
        allowed_senders: Vec::new(),
    }
}

#[test]
fn validation_rejects_hostile_hosts_ports_bots_and_senders() {
    assert!(valid().validate().is_ok());
    for host in [
        "imap.example.com\r\nA1 LOGIN x y",
        "imap example.com",
        "imap.example.com/path",
        "user@imap.example.com",
        "http://imap.example.com",
        "imap.example.com:993",
        "-bad.example.com",
        ".bad",
        "a..b",
        "caf\u{e9}.example.com",
        "tab\there",
        "",
        &"a".repeat(254),
    ] {
        let mut a = valid();
        a.imap.host = host.into();
        assert!(a.validate().is_err(), "imap host {host:?} must be refused");
        let mut a = valid();
        a.smtp.host = host.into();
        assert!(a.validate().is_err(), "smtp host {host:?} must be refused");
    }
    for ok in [
        "imap.gmail.com",
        "localhost",
        "127.0.0.1",
        "mail-1.example.org",
        "::1",
    ] {
        assert!(validate_host(ok, "x").is_ok(), "{ok}");
    }
    let mut a = valid();
    a.username = "u".repeat(255);
    assert!(a.validate().is_err(), "username length cap");
    let mut a = valid();
    a.address = format!("{}@example.com", "a".repeat(250));
    assert!(a.validate().is_err(), "address length cap");
    let mut a = valid();
    a.imap.port = 0;
    assert!(a.validate().is_err());
    let mut a = valid();
    a.username = "me\u{7}".into();
    assert!(a.validate().is_err());
    for bot in ["", "../x", "a b", &"a".repeat(65), "bot\n"] {
        let mut a = valid();
        a.bot = Some(bot.into());
        assert!(a.validate().is_err(), "bot {bot:?}");
    }
    let mut a = valid();
    a.bot = Some(BOT_A.into());
    assert!(a.validate().is_ok());
    for s in [
        "not an address",
        "a@b.c\r\nBcc: x@y.z",
        "@nodot",
        "@",
        "x@",
        "",
    ] {
        let mut a = valid();
        a.allowed_senders = vec![s.into()];
        assert!(a.validate().is_err(), "sender {s:?}");
    }
    let mut a = valid();
    a.allowed_senders = (0..101).map(|i| format!("u{i}@example.com")).collect();
    assert!(a.validate().is_err());
    let mut a = valid();
    a.allowed_senders = vec!["Boss@Example.com".into(), "@corp.example".into()];
    assert!(a.validate().is_ok());
    assert_eq!(
        normalize_sender(" Boss@Example.COM ").unwrap(),
        "boss@example.com"
    );
    assert_eq!(normalize_sender("@Corp.Example").unwrap(), "@corp.example");
    assert!(validate_account_name("work-1.a_b").is_ok());
    for n in ["", "a b", "a/b", ".x", "x\n", &"a".repeat(65)] {
        assert!(validate_account_name(n).is_err(), "{n:?}");
    }
}
