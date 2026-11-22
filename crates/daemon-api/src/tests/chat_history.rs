//! History reads must preserve owner scope and redact before serialization/snipping.
use super::*;
use protocol::{BotId, ChatId, RunId};
use uuid::Uuid;

fn message(t: &T, chat: &str, text: &str, private: bool, role: &str) -> Uuid {
    let id = Uuid::now_v7();
    t.state
        .storage()
        .insert_message(&storage::Message {
            id,
            user_id: t.state.user_id(),
            chat_id: ChatId(chat.parse().unwrap()),
            run_id: None,
            role: role.into(),
            content: text.into(),
            created_at_ms: 0,
            exposure_untrusted: false,
            exposure_private: private,
            exposure_sources: vec![],
        })
        .unwrap();
    id
}

#[tokio::test]
async fn search_is_owner_only_scoped_bounded_and_never_matches_secrets() {
    let redactor = agent_core::SharedRedactor::default();
    redactor.register("history-secret-canary-value");
    let t = setup_with_config(
        |_| {},
        LoopConfig {
            redactor: Some(redactor),
            ..LoopConfig::default()
        },
    );
    let (bot, chat) = make_chat(&t).await;
    let hit = message(
        &t,
        &chat,
        &format!(
            "{} ancient English phrase {}",
            "prefix ".repeat(200),
            "suffix ".repeat(200)
        ),
        false,
        "assistant",
    );
    message(&t, &chat, "history-secret-canary-value", false, "user");
    message(&t, &chat, "ancient English phrase private", true, "user");
    message(&t, &chat, "ancient English phrase tool", false, "tool");
    let path = format!(
        "/v1/chats/search?q=ancient%20English%20phrase&bot_id={bot}&chat_id={chat}&limit=1"
    );
    assert_eq!(
        call_token_only(&t, Method::GET, &path, None).await.status,
        StatusCode::FORBIDDEN
    );
    let r = call(&t, Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    assert_eq!(r.json()["results"][0]["message_id"], hit.to_string());
    assert!(
        r.json()["results"][0]["snippet"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 320
    );
    let r = call(
        &t,
        Method::GET,
        "/v1/chats/search?q=history-secret-canary-value",
        None,
    )
    .await;
    assert_eq!(r.json()["results"], json!([]));
    assert_eq!(
        call(&t, Method::GET, "/v1/chats/search?q=x&limit=101", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // A page whose hits are all omitted by redaction does not hide later matches.
    let (_, quiet) = make_chat(&t).await;
    for _ in 0..3 {
        message(
            &t,
            &quiet,
            "zebra history-secret-canary-value",
            false,
            "user",
        );
    }
    let visible = message(&t, &quiet, "zebra and visible text", false, "assistant");
    let r = call(&t, Method::GET, "/v1/chats/search?q=zebra&limit=1", None).await;
    assert_eq!(r.json()["results"][0]["message_id"], visible.to_string());
    let (_, other) = make_chat(&t).await;
    let r = call(
        &t,
        Method::GET,
        &format!("/v1/chats/search?q=ancient&chat_id={other}"),
        None,
    )
    .await;
    assert_eq!(r.json()["results"], json!([]));
    call(&t, Method::DELETE, &format!("/v1/chats/{chat}"), None).await;
    assert_eq!(
        call(&t, Method::GET, "/v1/chats/search?q=ancient", None)
            .await
            .json()["results"],
        json!([])
    );
}

#[tokio::test]
async fn export_redacts_credentials_and_private_content_and_only_summarizes_tools() {
    const CANARY: &str = "history-secret-canary-value";
    let redactor = agent_core::SharedRedactor::default();
    redactor.register(CANARY);
    let t = setup_with_config(
        |_| {},
        LoopConfig {
            redactor: Some(redactor),
            ..LoopConfig::default()
        },
    );
    let (bot, chat) = make_chat(&t).await;
    let attachment = agent_core::attachments::store(
        t.state.storage().data_dir(),
        ChatId(chat.parse().unwrap()),
        "notes.txt",
        b"attachment-body-canary",
        &|v| v,
    )
    .unwrap();
    let text = agent_core::attachments::with_marker(&format!("visible {CANARY}"), &[attachment.id]);
    message(&t, &chat, &text, false, "user");
    let private = message(&t, &chat, "private-content-canary", true, "assistant");
    t.state
        .storage()
        .set_message_source(private, "imported")
        .unwrap();
    message(&t, &chat, "sensitive-tool-output", false, "tool");
    let old = message(&t, &chat, "superseded-canary", false, "user");
    t.state
        .storage()
        .set_messages_superseded(&[old], true)
        .unwrap();
    tool_fixture(&t, &bot, &chat, CANARY);
    for format in ["json", "markdown"] {
        let path = format!("/v1/chats/{chat}/export?format={format}");
        assert_eq!(
            call_token_only(&t, Method::GET, &path, None).await.status,
            StatusCode::FORBIDDEN
        );
        let r = call(&t, Method::GET, &path, None).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let text = String::from_utf8(r.body.clone()).unwrap();
        for forbidden in [
            CANARY,
            "private-content-canary",
            "tool-output-canary",
            "sensitive-tool-output",
            "superseded-canary",
            "attachment-body-canary",
        ] {
            assert!(!text.contains(forbidden), "leaked {forbidden}");
        }
        assert!(
            text.contains("[REDACTED]") && text.contains("read_file") && text.contains("denied")
        );
        assert_eq!(r.headers["cache-control"], "no-store");
        if format == "json" {
            assert_eq!(r.json()["format_version"], 1);
            assert_eq!(r.json()["messages"][0]["attachments"], json!(["notes.txt"]));
            assert_eq!(r.json()["messages"].as_array().unwrap().len(), 2);
            assert!(
                r.json()["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["source"] == "imported")
            );
            assert_eq!(r.json()["steps"].as_array().unwrap().len(), 1);
        }
    }
    message(&t, &chat, &"x".repeat(2 * 1024 * 1024 + 1), false, "user");
    assert_eq!(
        call(&t, Method::GET, &format!("/v1/chats/{chat}/export"), None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn import_creates_untrusted_history_and_never_runs_tool_rows() {
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let run = RunId::new();
    let doc = json!({
        "format_version": 1,
        "chat_id": Uuid::from_u128(90),
        "title": "Imported notes",
        "created_at_ms": 0,
        "messages": [
            {"id": Uuid::from_u128(91), "run_id": run, "role": "user", "created_at_ms": 0, "content": "hello"},
            {"role": "assistant", "content": "hi"}
        ],
        "steps": [{"run_id": run, "name": "shell_run", "status": "succeeded", "created_at_ms": 0}],
        "approvals": [{"run_id": run, "outcome": "allowed", "created_at_ms": 0}]
    });
    let path = format!("/v1/chats/import?bot_id={bot}");
    assert_eq!(
        call_token_only(&t, Method::POST, &path, Some(doc.clone()))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let r = call(&t, Method::POST, &path, Some(doc.clone())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    let chat = r.json()["chat_id"].as_str().unwrap().to_owned();
    assert_eq!(r.json()["messages"], 2);
    assert_eq!(r.json()["steps_ignored"], 1);
    assert_eq!(r.json()["approvals_ignored"], 1);

    let chat_id = ChatId(chat.parse().unwrap());
    let messages = t.state.storage().list_messages(chat_id).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[2].role, agent_core::control_note::IMPORTED_ROLE);
    for message in &messages {
        assert!(message.exposure_untrusted);
        assert_eq!(
            t.state.storage().message_source(message.id).unwrap(),
            "imported"
        );
    }
    assert_eq!(
        t.state
            .storage()
            .chat_step_summaries(t.state.user_id(), chat_id)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        t.state
            .storage()
            .chat_approval_summaries(t.state.user_id(), chat_id)
            .unwrap()
            .len(),
        0
    );

    let mut newer = doc.clone();
    newer["format_version"] = json!(2);
    assert_eq!(
        call(&t, Method::POST, &path, Some(newer)).await.status,
        StatusCode::BAD_REQUEST
    );
    let mut forged_role = doc.clone();
    forged_role["messages"][0]["role"] = json!("tool");
    assert_eq!(
        call(&t, Method::POST, &path, Some(forged_role))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let mut too_many = doc;
    too_many["messages"] = json!(vec![json!({"role":"user","content":""}); 1001]);
    assert_eq!(
        call(&t, Method::POST, &path, Some(too_many)).await.status,
        StatusCode::BAD_REQUEST
    );
}

fn tool_fixture(t: &T, bot: &str, chat: &str, canary: &str) {
    let st = t.state.storage();
    let run = RunId::new();
    st.insert_run(&storage::Run {
        id: run,
        user_id: t.state.user_id(),
        bot_id: BotId(bot.parse().unwrap()),
        chat_id: Some(ChatId(chat.parse().unwrap())),
        kind: "turn".into(),
        state: "succeeded".into(),
        error: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        parent_run_id: None,
        spec: None,
        computer_target: None,
        computer_node: None,
        computer_node_name: None,
        paused_at_ms: None,
        pause_reason: None,
    })
    .unwrap();
    st.insert_step(&storage::RunStep {
        id: Uuid::now_v7(),
        run_id: run,
        user_id: t.state.user_id(),
        idx: 0,
        name: "tools".into(),
        input: json!({"calls":[{"name":"read_file","arguments":{"secret":canary}}]}),
        idempotent: true,
        retry: None,
        status: "succeeded".into(),
        attempt: 1,
        output: Some(json!({"sensitive": "tool-output-canary"})),
        error: None,
        started_at_ms: 0,
        finished_at_ms: Some(1),
    })
    .unwrap();
    st.insert_approval(&storage::Approval {
        id: Uuid::now_v7(),
        user_id: t.state.user_id(),
        run_id: run,
        step_id: None,
        summary: canary.into(),
        decision: Some("denied".into()),
        requested_at_ms: 0,
        decided_at_ms: Some(1),
    })
    .unwrap();
}
