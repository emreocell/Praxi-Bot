use std::sync::Arc;

use secret_store::{MemoryStore, SecretStore};
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, ToolContext, ToolError, ToolRegistry,
};

use crate::testutil::{Protocol, account, connector, server};
use crate::{EmailConnector, EmailError, OutgoingMail, SearchQuery, SmtpSecurity, register, tools};

fn mail() -> OutgoingMail {
    OutgoingMail {
        to: vec!["bob@example.com".into()],
        bcc: vec!["hidden@example.com".into()],
        subject: "Test submission".into(),
        body: "Hello fixture".into(),
        ..Default::default()
    }
}

async fn call(conn: &Arc<EmailConnector>, name: &str, input: Value) -> Value {
    let tool = tools(conn)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap();
    let output = tool.call(&ToolContext::default(), input).await.unwrap();
    let ContentPart::Json { data } = &output.content[0] else {
        panic!("expected JSON")
    };
    data.clone()
}

#[tokio::test]
async fn imap_tools_list_search_read_mark_and_draft_over_tls() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let conn = connector(account(&imap, &smtp, SmtpSecurity::Implicit));
    let folders = call(&conn, "email_list_folders", json!({})).await;
    assert_eq!(folders["folders"][0]["name"], "INBOX");
    let result = call(
        &conn,
        "email_search",
        json!({
            "from":"ann@example.com", "subject":"Fixture", "since":"2026-11-01",
            "before":"2026-12-01", "unread":true, "limit":2
        }),
    )
    .await;
    assert_eq!(result["total_matches"], 2);
    assert_eq!(result["messages"][0]["uid"], 42);
    assert_eq!(result["messages"][1]["uid"], 7);
    assert_eq!(result["messages"][0]["subject"], "Fixture");
    let detail = call(&conn, "email_read", json!({"uid":42,"max_body_chars":5})).await;
    assert_eq!(detail["untrusted_content"], true);
    assert_eq!(detail["message"]["body_text"], "Hello");
    assert_eq!(detail["message"]["body_truncated"], true);
    assert_eq!(detail["message"]["attachments"][0]["filename"], "a.pdf");
    assert_eq!(detail["message"]["attachments"][0]["size"], 4);
    assert!(matches!(
        conn.fetch(None, "INBOX", 999, 100).await,
        Err(EmailError::NotFound(_))
    ));
    call(&conn, "email_mark_read", json!({"uid":42})).await;
    call(&conn, "email_mark_read", json!({"uid":42,"read":false})).await;
    let draft = call(
        &conn,
        "email_draft",
        json!({
            "account":"test", "to":["bob@example.com"], "bcc":["hidden@example.com"], "subject":"Draft", "body":"Unsent"
        }),
    )
    .await;
    assert_eq!(draft["sent"], false);
    assert_eq!(draft["folder"], "Drafts");
    assert!(smtp.commands.lock().unwrap().is_empty());
    let messages = imap.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(String::from_utf8_lossy(&messages[0]).contains("Subject: Draft"));
    assert!(String::from_utf8_lossy(&messages[0]).contains("hidden@example.com"));
    drop(messages);
    let commands = imap.commands.lock().unwrap();
    assert!(commands.iter().any(|c| c.contains("UID SEARCH FROM \"ann@example.com\" SUBJECT \"Fixture\" SINCE 1-Nov-2026 BEFORE 1-Dec-2026 UNSEEN")));
    assert!(commands.iter().any(|c| c.contains("BODY.PEEK[]")));
    assert!(commands.iter().any(|c| c.contains("BODY.PEEK[HEADER]")));
    assert!(commands.iter().any(|c| c.contains("+FLAGS (\\Seen)")));
    assert!(commands.iter().any(|c| c.contains("-FLAGS (\\Seen)")));
    assert!(
        commands
            .iter()
            .any(|c| c.contains("APPEND") && c.contains("\\Draft"))
    );
}

#[tokio::test]
async fn smtp_implicit_and_required_starttls_send_with_bcc_only_in_envelope() {
    for (protocol, security) in [
        (Protocol::Smtp, SmtpSecurity::Implicit),
        (Protocol::StartTls, SmtpSecurity::StartTls),
    ] {
        let imap = server(Protocol::Imap).await;
        let smtp = server(protocol).await;
        let conn = connector(account(&imap, &smtp, security));
        // The host invokes this only after its approval gate has granted send.
        let result = call(
            &conn,
            "email_send",
            json!({
                "to":["bob@example.com"], "bcc":["hidden@example.com"],
                "subject":"Test submission", "body":"Hello fixture"
            }),
        )
        .await;
        assert_eq!(result["sent"], true);
        assert_eq!(result["recipients"], 2);
        let commands = smtp.commands.lock().unwrap();
        assert!(commands.iter().any(|c| c.starts_with("AUTH PLAIN")));
        assert!(
            commands
                .iter()
                .any(|c| c.contains("RCPT TO:<hidden@example.com>"))
        );
        if security == SmtpSecurity::StartTls {
            let tls = commands.iter().position(|c| c == "STARTTLS\r\n").unwrap();
            let auth = commands.iter().position(|c| c.starts_with("AUTH")).unwrap();
            assert!(tls < auth);
        }
        let messages = smtp.messages.lock().unwrap();
        assert_eq!(messages.len(), 1);
        let raw = String::from_utf8_lossy(&messages[0]);
        assert!(raw.contains("Subject: Test submission"));
        assert!(!raw.to_ascii_lowercase().contains("bcc:"));
        assert!(!raw.contains("hidden@example.com"));
    }
}

#[tokio::test]
async fn tls_verification_and_starttls_downgrade_fail_closed() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let mut config = account(&imap, &smtp, SmtpSecurity::Implicit);
    config.extra_root_pem = None;
    let conn = connector(config);
    assert!(matches!(
        conn.list_folders(None).await,
        Err(EmailError::Tls(_))
    ));
    assert!(conn.send(None, &mail()).await.is_err());
    assert!(imap.commands.lock().unwrap().is_empty());
    assert!(smtp.commands.lock().unwrap().is_empty());
    let smtp = server(Protocol::NoStartTls).await;
    let conn = connector(account(&imap, &smtp, SmtpSecurity::StartTls));
    assert!(conn.send(None, &mail()).await.is_err());
    assert!(
        !smtp
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("AUTH") || c.starts_with("MAIL"))
    );
}

#[tokio::test]
async fn missing_credentials_auth_failure_and_timeout() {
    let imap = server(Protocol::RejectLogin).await;
    let smtp = server(Protocol::Smtp).await;
    let config = account(&imap, &smtp, SmtpSecurity::Implicit);
    let store = Arc::new(MemoryStore::new());
    let conn = EmailConnector::new(vec![config.clone()], store.clone()).unwrap();
    assert!(matches!(
        conn.list_folders(None).await,
        Err(EmailError::Credential(_))
    ));
    assert!(imap.commands.lock().unwrap().is_empty());
    store.set("email/test", "test-app-password").unwrap();
    let error = conn.list_folders(None).await.unwrap_err().to_string();
    assert!(error.contains("login failed"));
    assert!(!error.contains("test-app-password"));
    let stall = server(Protocol::Stall).await;
    let mut config = account(&stall, &smtp, SmtpSecurity::Implicit);
    config.timeout_secs = 1;
    let conn = connector(config);
    assert!(matches!(
        conn.list_folders(None).await,
        Err(EmailError::Timeout(1))
    ));
}

#[tokio::test]
async fn registration_effects_and_input_validation() {
    let imap = server(Protocol::Imap).await;
    let smtp = server(Protocol::Smtp).await;
    let config = account(&imap, &smtp, SmtpSecurity::Implicit);
    let conn = connector(config.clone());
    let registry = ToolRegistry::new();
    assert_eq!(register(&registry, &conn), 6);
    assert_eq!(register(&registry, &conn), 0);
    for name in crate::TOOL_NAMES {
        let tool = registry.get(name).unwrap();
        assert!(tool.spec().is_valid());
        assert_eq!(tool.spec().category, PermissionCategory::Messaging);
        let expected = match name {
            "email_send" => Effect::Destructive,
            "email_draft" | "email_mark_read" => Effect::Write,
            _ => Effect::Read,
        };
        assert_eq!(tool.spec().effect, expected);
    }
    for (name, input) in [
        ("email_read", json!({})),
        ("email_mark_read", json!({"uid":-1})),
        ("email_send", json!({"to":["bad address"]})),
        (
            "email_send",
            json!({"to":["bob@example.com"], "password":"inline-not-allowed"}),
        ),
        ("email_search", json!({"from":"a\r\nDELETE INBOX"})),
        ("email_search", json!({"since":"2026-02-30"})),
    ] {
        let err = registry
            .get(name)
            .unwrap()
            .call(&ToolContext::default(), input)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{name}: {err}");
    }
    assert!(imap.commands.lock().unwrap().is_empty());
    let store = Arc::new(MemoryStore::new());
    assert!(EmailConnector::new(vec![], store.clone()).is_err());
    assert!(EmailConnector::new(vec![config.clone(), config.clone()], store.clone()).is_err());
    let mut second = config.clone();
    second.name = "second".into();
    let multi = EmailConnector::new(vec![config, second], store).unwrap();
    assert!(matches!(
        multi.list_folders(None).await,
        Err(EmailError::Input(_))
    ));
    assert!(matches!(
        multi.list_folders(Some("missing")).await,
        Err(EmailError::Input(_))
    ));
    assert_eq!(
        SearchQuery {
            unread: Some(false),
            ..Default::default()
        }
        .to_imap()
        .unwrap(),
        "SEEN"
    );
}
