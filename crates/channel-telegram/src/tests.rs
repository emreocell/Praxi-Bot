//! Hermetic tests for `channel-telegram` using a local `wiremock` Telegram Bot API server.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use parking_lot::Mutex;
use protocol::event::{
    ApprovalNeeded, MessageDelta, QuestionAsked, RunFinished, ToolCallCompleted, ToolCallRequested,
};
use protocol::{BotId, ChatId, Envelope, Event, RunId, ToolCallId};
use secret_store::{MemoryStore, REDACTED, SecretStore, SecretValue};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::{
    ApprovalDecision, ApprovalScope, BotSummary, Channel, ChannelCaps, ChatRef, ChatSummary,
    DaemonClient, DaemonStatus, DeliveryKind, HttpDaemonClient, IncomingKind, OutgoingMessage,
    Result, SendOutcome, TELEGRAM_BOT_TOKEN_SECRET, TelegramChannel, TelegramConfig,
    ThrottledEditor, Update, store_bot_token,
};

const TEST_BOT_TOKEN: &str = "123456789:AAH_test_bot_token_secret_value_xyz";
const PAIRED_USER_ID: i64 = 4242;
const UNPAIRED_USER_ID: i64 = 9999;
const TEST_CHAT_ID: i64 = 7001;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedApproval {
    call_id: ToolCallId,
    decision: ApprovalDecision,
    scope: ApprovalScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedQuestionAnswer {
    call_id: ToolCallId,
    answer: String,
}

struct FakeDaemon {
    bots: Vec<BotSummary>,
    created_chats: Mutex<Vec<(BotId, Option<String>, ChatId)>>,
    sent_messages: Mutex<Vec<(ChatId, String)>>,
    approvals: Mutex<Vec<RecordedApproval>>,
    answers: Mutex<Vec<RecordedQuestionAnswer>>,
    cancelled_runs: Mutex<Vec<RunId>>,
    run_streams: Mutex<VecDeque<(RunId, BoxStream<'static, Envelope>)>>,
}

impl FakeDaemon {
    fn new(bots: Vec<BotSummary>) -> Self {
        Self {
            bots,
            created_chats: Mutex::new(Vec::new()),
            sent_messages: Mutex::new(Vec::new()),
            approvals: Mutex::new(Vec::new()),
            answers: Mutex::new(Vec::new()),
            cancelled_runs: Mutex::new(Vec::new()),
            run_streams: Mutex::new(VecDeque::new()),
        }
    }

    fn queue_run_events(&self, run_id: RunId, events: Vec<Event>) {
        let envs: Vec<Envelope> = events
            .into_iter()
            .enumerate()
            .map(|(i, ev)| Envelope::new(i as u64, ev))
            .collect();
        self.run_streams
            .lock()
            .push_back((run_id, Box::pin(stream::iter(envs))));
    }

    fn queue_run_stream(&self, run_id: RunId, stream: BoxStream<'static, Envelope>) {
        self.run_streams.lock().push_back((run_id, stream));
    }
}

#[async_trait]
impl DaemonClient for FakeDaemon {
    async fn list_bots(&self) -> Result<Vec<BotSummary>> {
        Ok(self.bots.clone())
    }

    async fn create_chat(&self, bot_id: BotId, title: Option<&str>) -> Result<ChatSummary> {
        let chat_id = ChatId::new();
        let t = title.unwrap_or("New chat").to_owned();
        self.created_chats
            .lock()
            .push((bot_id, title.map(str::to_owned), chat_id));
        Ok(ChatSummary {
            id: chat_id,
            bot_id,
            title: t,
        })
    }

    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<SendOutcome> {
        self.sent_messages.lock().push((chat_id, text.to_owned()));
        let run_id = self
            .run_streams
            .lock()
            .front()
            .map_or_else(RunId::new, |(r, _)| *r);
        Ok(SendOutcome {
            delivery: DeliveryKind::NewTurn,
            run_id: Some(run_id),
        })
    }

    async fn decide_approval(
        &self,
        call_id: ToolCallId,
        decision: ApprovalDecision,
        scope: ApprovalScope,
        _reason: Option<&str>,
    ) -> Result<()> {
        self.approvals.lock().push(RecordedApproval {
            call_id,
            decision,
            scope,
        });
        Ok(())
    }

    async fn answer_question(&self, call_id: ToolCallId, answer: &str) -> Result<()> {
        self.answers.lock().push(RecordedQuestionAnswer {
            call_id,
            answer: answer.to_owned(),
        });
        Ok(())
    }

    async fn cancel_run(&self, run_id: RunId) -> Result<()> {
        self.cancelled_runs.lock().push(run_id);
        Ok(())
    }

    async fn status(&self) -> Result<DaemonStatus> {
        Ok(DaemonStatus {
            status: "ok".into(),
            version: "0.1.0".into(),
            uptime_secs: 120,
            pending_approvals: 1,
        })
    }

    async fn subscribe_run(&self, run_id: RunId) -> Result<BoxStream<'static, Envelope>> {
        let mut guard = self.run_streams.lock();
        if let Some(pos) = guard.iter().position(|(r, _)| *r == run_id) {
            let (_, s) = guard.remove(pos).expect("position exists");
            return Ok(s);
        }
        let end = Envelope::new(
            0,
            Event::RunFinished(RunFinished {
                run_id,
                error: None,
            }),
        );
        Ok(Box::pin(stream::iter(vec![end])))
    }
}

fn tg_endpoint(method_name: &str) -> String {
    format!("/bot{TEST_BOT_TOKEN}/{method_name}")
}

fn text_update(update_id: i64, user_id: i64, chat_id: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id * 10,
            "from": {
                "id": user_id,
                "is_bot": false,
                "first_name": "Tester",
                "username": "tester"
            },
            "chat": {
                "id": chat_id,
                "type": "private"
            },
            "text": text
        }
    })
}

fn callback_update(
    update_id: i64,
    user_id: i64,
    chat_id: i64,
    query_id: &str,
    data: &str,
) -> Value {
    json!({
        "update_id": update_id,
        "callback_query": {
            "id": query_id,
            "from": {
                "id": user_id,
                "is_bot": false,
                "first_name": "Tester"
            },
            "message": {
                "message_id": 500,
                "chat": {
                    "id": chat_id,
                    "type": "private"
                },
                "text": "prompt"
            },
            "data": data
        }
    })
}

async fn mount_telegram_mocks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let chat_id = body["chat_id"].as_i64().unwrap_or(TEST_CHAT_ID);
            let text = body["text"].as_str().unwrap_or("");
            ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": {
                    "message_id": 101,
                    "chat": { "id": chat_id, "type": "private" },
                    "text": text
                }
            }))
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path(tg_endpoint("editMessageText")))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let chat_id = body["chat_id"].as_i64().unwrap_or(TEST_CHAT_ID);
            let message_id = body["message_id"].as_i64().unwrap_or(101);
            let text = body["text"].as_str().unwrap_or("");
            ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": {
                    "message_id": message_id,
                    "chat": { "id": chat_id, "type": "private" },
                    "text": text
                }
            }))
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path(tg_endpoint("answerCallbackQuery")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": true
        })))
        .mount(server)
        .await;
}

fn sent_messages(requests: &[Request]) -> Vec<Value> {
    let target = tg_endpoint("sendMessage");
    requests
        .iter()
        .filter(|r| r.url.path() == target)
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .collect()
}

fn edited_messages(requests: &[Request]) -> Vec<Value> {
    let target = tg_endpoint("editMessageText");
    requests
        .iter()
        .filter(|r| r.url.path() == target)
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .collect()
}

fn callback_answers(requests: &[Request]) -> Vec<Value> {
    let target = tg_endpoint("answerCallbackQuery");
    requests
        .iter()
        .filter(|r| r.url.path() == target)
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .collect()
}

#[tokio::test]
async fn pairing_flow_valid_wrong_expired_and_single_use() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("telegram_state.json");

    let store = Arc::new(MemoryStore::new());
    let token_ref = store_bot_token(&*store, TEST_BOT_TOKEN).unwrap();
    assert_eq!(token_ref.name(), TELEGRAM_BOT_TOKEN_SECRET);

    let daemon = Arc::new(FakeDaemon::new(vec![BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    }]));

    let cfg = TelegramConfig::new(store.clone())
        .with_api_base_url(server.uri())
        .with_state_path(&state_file);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    // 1. Expired code is rejected.
    let expired_code = channel.create_pair_code_with_ttl(Duration::ZERO);
    assert_eq!(expired_code.len(), 6);
    assert!(expired_code.chars().all(|c| c.is_ascii_digit()));

    let u1: Update = serde_json::from_value(text_update(
        1,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        &format!("/pair {expired_code}"),
    ))
    .unwrap();
    channel.handle_update(u1).await.unwrap();
    assert!(!channel.is_paired(PAIRED_USER_ID));

    // 2. Wrong code is rejected without consuming the valid code.
    let valid_code = channel.create_pair_code();
    let wrong_code = if valid_code == "000000" {
        "999999"
    } else {
        "000000"
    };
    let u2: Update = serde_json::from_value(text_update(
        2,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        &format!("/pair {wrong_code}"),
    ))
    .unwrap();
    channel.handle_update(u2).await.unwrap();
    assert!(!channel.is_paired(PAIRED_USER_ID));

    // 3. Valid code pairs the user and persists their ID.
    let u3: Update = serde_json::from_value(text_update(
        3,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        &format!("/pair {valid_code}"),
    ))
    .unwrap();
    channel.handle_update(u3).await.unwrap();
    assert!(channel.is_paired(PAIRED_USER_ID));

    // 4. Reusing the same one-time code from another user fails.
    let u4: Update = serde_json::from_value(text_update(
        4,
        UNPAIRED_USER_ID,
        TEST_CHAT_ID,
        &format!("/pair {valid_code}"),
    ))
    .unwrap();
    channel.handle_update(u4).await.unwrap();
    assert!(!channel.is_paired(UNPAIRED_USER_ID));

    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    assert_eq!(msgs.len(), 4);
    assert!(msgs[0]["text"].as_str().unwrap().contains("expired"));
    assert!(msgs[1]["text"].as_str().unwrap().contains("Invalid"));
    assert!(msgs[2]["text"].as_str().unwrap().contains("Paired"));
    assert!(msgs[3]["text"].as_str().unwrap().contains("Invalid"));

    // 5. Reopening from state_file preserves the paired user.
    let reopened = TelegramChannel::new(
        TelegramConfig::new(store)
            .with_api_base_url(server.uri())
            .with_state_path(&state_file),
        daemon,
    )
    .unwrap();
    assert!(reopened.is_paired(PAIRED_USER_ID));
}

#[tokio::test]
async fn unpaired_user_receives_single_refusal_then_is_ignored() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let daemon = Arc::new(FakeDaemon::new(vec![BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    }]));

    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    // Unpaired user sends three messages in a row.
    for (idx, text) in [(1, "hello"), (2, "run shell rm -rf"), (3, "/bots")] {
        let u: Update =
            serde_json::from_value(text_update(idx, UNPAIRED_USER_ID, 8888, text)).unwrap();
        channel.handle_update(u).await.unwrap();
    }

    // Only one polite refusal was sent; the subsequent two messages were ignored.
    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    assert_eq!(
        msgs.len(),
        1,
        "unpaired user must only get a single refusal"
    );
    assert!(
        msgs[0]["text"]
            .as_str()
            .unwrap()
            .contains("/pair <6-digit-code>")
    );
    assert!(
        daemon.sent_messages.lock().is_empty(),
        "unpaired user must never reach the daemon"
    );
}

#[tokio::test]
async fn message_and_commands_map_to_daemon_api_calls() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let bot_a = BotSummary {
        id: BotId::new(),
        name: "Alpha".into(),
    };
    let bot_b = BotSummary {
        id: BotId::new(),
        name: "Beta".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot_a.clone(), bot_b.clone()]));

    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    // 1. `/bots` lists both bots.
    let u_bots: Update =
        serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "/bots")).unwrap();
    channel.handle_update(u_bots).await.unwrap();

    // 2. `/use Beta` switches the chat's active bot to Beta.
    let u_use: Update =
        serde_json::from_value(text_update(2, PAIRED_USER_ID, TEST_CHAT_ID, "/use Beta")).unwrap();
    channel.handle_update(u_use).await.unwrap();
    assert_eq!(channel.pairing().bot_for_chat(TEST_CHAT_ID), Some(bot_b.id));

    // 3. Incoming text maps to `create_chat` + `send_message` for Beta and streams the reply.
    let run_id = RunId::new();
    daemon.queue_run_events(
        run_id,
        vec![
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Hello from Beta!".into(),
            }),
            Event::RunFinished(RunFinished {
                run_id,
                error: None,
            }),
        ],
    );
    let u_msg: Update = serde_json::from_value(text_update(
        3,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "summarize my inbox",
    ))
    .unwrap();
    channel.handle_update(u_msg).await.unwrap();

    {
        let chats = daemon.created_chats.lock();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].0, bot_b.id);
        let sent = daemon.sent_messages.lock();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, chats[0].2);
        assert_eq!(sent[0].1, "summarize my inbox");
    }

    // 4. `/status` queries daemon health and pending approvals.
    let u_status: Update =
        serde_json::from_value(text_update(4, PAIRED_USER_ID, TEST_CHAT_ID, "/status")).unwrap();
    channel.handle_update(u_status).await.unwrap();

    // 5. `/stop` cancels an in-progress run.
    let long_run = RunId::new();
    let (_tx, rx) = mpsc::unbounded_channel::<Envelope>();
    let pending_stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    daemon.queue_run_stream(long_run, Box::pin(pending_stream));

    let u_start: Update =
        serde_json::from_value(text_update(5, PAIRED_USER_ID, TEST_CHAT_ID, "long task")).unwrap();
    channel.handle_update(u_start).await.unwrap();

    let u_stop: Update =
        serde_json::from_value(text_update(6, PAIRED_USER_ID, TEST_CHAT_ID, "/stop")).unwrap();
    channel.handle_update(u_stop).await.unwrap();
    assert_eq!(*daemon.cancelled_runs.lock(), vec![long_run]);

    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    assert!(
        msgs.iter()
            .any(|m| m["text"].as_str().unwrap().contains("Alpha")
                && m["text"].as_str().unwrap().contains("Beta"))
    );
    assert!(msgs.iter().any(|m| {
        m["text"]
            .as_str()
            .unwrap()
            .contains("Switched active bot to Beta")
    }));
    assert!(
        msgs.iter()
            .any(|m| m["text"].as_str().unwrap() == "Hello from Beta!")
    );
    assert!(msgs.iter().any(|m| {
        m["text"]
            .as_str()
            .unwrap()
            .contains("Daemon: ok (v0.1.0, up 120s)")
    }));
    assert!(
        msgs.iter()
            .any(|m| m["text"].as_str().unwrap().contains("Stopped run"))
    );
}

#[tokio::test]
async fn approval_buttons_map_to_daemon_decisions() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let bot = BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot]));

    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let approval_call_id = ToolCallId::new();

    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::ApprovalNeeded(ApprovalNeeded {
            run_id,
            call_id: approval_call_id,
            tool_name: "fs_delete".into(),
            input: json!({ "path": "/tmp/a.txt" }),
            summary: "Delete /tmp/a.txt".into(),
        }),
    ))
    .unwrap();

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    daemon.queue_run_stream(run_id, Box::pin(stream));

    let u1: Update =
        serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "delete a.txt"))
            .unwrap();
    channel.handle_update(u1).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    assert_eq!(msgs.len(), 1);
    let keyboard = &msgs[0]["reply_markup"]["inline_keyboard"][0];
    assert_eq!(keyboard[0]["text"], "Approve once");
    assert_eq!(keyboard[1]["text"], "Always");
    assert_eq!(keyboard[2]["text"], "Deny");

    let approve_once_data = keyboard[0]["callback_data"].as_str().unwrap().to_owned();
    let always_data = keyboard[1]["callback_data"].as_str().unwrap().to_owned();
    let deny_data = keyboard[2]["callback_data"].as_str().unwrap().to_owned();

    for (idx, qid, data) in [
        (2, "cb-1", &approve_once_data),
        (3, "cb-2", &always_data),
        (4, "cb-3", &deny_data),
    ] {
        let cb: Update = serde_json::from_value(callback_update(
            idx,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            qid,
            data,
        ))
        .unwrap();
        channel.handle_update(cb).await.unwrap();
    }

    let approvals = daemon.approvals.lock().clone();
    assert_eq!(
        approvals,
        vec![
            RecordedApproval {
                call_id: approval_call_id,
                decision: ApprovalDecision::Approve,
                scope: ApprovalScope::Once,
            },
            RecordedApproval {
                call_id: approval_call_id,
                decision: ApprovalDecision::Approve,
                scope: ApprovalScope::Always,
            },
            RecordedApproval {
                call_id: approval_call_id,
                decision: ApprovalDecision::Deny,
                scope: ApprovalScope::Once,
            },
        ]
    );
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(callback_answers(&reqs).len(), 3);
}

#[tokio::test]
async fn question_asked_buttons_and_free_text_map_to_daemon_answers() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let bot = BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot]));

    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let q_button_id = ToolCallId::new();
    let q_text_id = ToolCallId::new();

    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::QuestionAsked(QuestionAsked {
            run_id,
            call_id: q_button_id,
            question: "Which format?".into(),
            options: vec!["PDF".into(), "Markdown".into()],
        }),
    ))
    .unwrap();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    daemon.queue_run_stream(run_id, Box::pin(stream));

    let u1: Update = serde_json::from_value(text_update(
        1,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "export report",
    ))
    .unwrap();
    channel.handle_update(u1).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    let md_button_data = msgs[0]["reply_markup"]["inline_keyboard"][1][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_owned();

    // After clicking "Markdown", the run asks a free-text question.
    tx.send(Envelope::new(
        1,
        Event::QuestionAsked(QuestionAsked {
            run_id,
            call_id: q_text_id,
            question: "What file name?".into(),
            options: Vec::new(),
        }),
    ))
    .unwrap();

    let cb_q: Update = serde_json::from_value(callback_update(
        2,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "cb-q",
        &md_button_data,
    ))
    .unwrap();
    channel.handle_update(cb_q).await.unwrap();

    // User answers the second question with free text.
    tx.send(Envelope::new(
        2,
        Event::RunFinished(RunFinished {
            run_id,
            error: None,
        }),
    ))
    .unwrap();
    drop(tx);

    let u_free: Update = serde_json::from_value(text_update(
        3,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "quarterly-summary.md",
    ))
    .unwrap();
    channel.handle_update(u_free).await.unwrap();

    let answers = daemon.answers.lock().clone();
    assert_eq!(
        answers,
        vec![
            RecordedQuestionAnswer {
                call_id: q_button_id,
                answer: "Markdown".into(),
            },
            RecordedQuestionAnswer {
                call_id: q_text_id,
                answer: "quarterly-summary.md".into(),
            },
        ]
    );
}

#[tokio::test]
async fn edit_throttling_caps_edits_to_at_most_one_per_second() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let bot = BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot]));

    // Default 1-second edit interval.
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_edit_interval(Duration::from_secs(1))
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    // 6 deltas arrive back-to-back within milliseconds, followed by RunFinished.
    daemon.queue_run_events(
        run_id,
        vec![
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 1 ".into(),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 2 ".into(),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 3 ".into(),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 4 ".into(),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 5 ".into(),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: "Part 6.".into(),
            }),
            Event::RunFinished(RunFinished {
                run_id,
                error: None,
            }),
        ],
    );

    let u: Update =
        serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "stream test"))
            .unwrap();
    channel.handle_update(u).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let sends = sent_messages(&reqs);
    let edits = edited_messages(&reqs);

    // First delta creates the message via `sendMessage`; intermediate deltas within 1s are
    // throttled; `RunFinished` performs a single final `editMessageText`.
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["text"], "Part 1 ");
    assert_eq!(
        edits.len(),
        1,
        "intermediate deltas within 1s must be throttled into a single final edit"
    );
    assert_eq!(
        edits[0]["text"],
        "Part 1 Part 2 Part 3 Part 4 Part 5 Part 6."
    );

    // Also verify ThrottledEditor across an elapsed interval window.
    let mut editor = ThrottledEditor::new(TEST_CHAT_ID, Duration::from_millis(30));
    editor.push(channel.bot_api(), "A").await.unwrap();
    editor.push(channel.bot_api(), "AB").await.unwrap();
    editor.push(channel.bot_api(), "ABC").await.unwrap();
    // Still inside 30ms window: only 1 edit so far (from the earlier run).
    let edits_before = edited_messages(&server.received_requests().await.unwrap()).len();
    assert_eq!(edits_before, 1);

    tokio::time::sleep(Duration::from_millis(40)).await;
    editor.push(channel.bot_api(), "ABCD").await.unwrap();
    let edits_after = edited_messages(&server.received_requests().await.unwrap());
    assert_eq!(edits_after.len(), 2);
    assert_eq!(edits_after[1]["text"], "ABCD");
}

#[tokio::test]
async fn redacts_stored_secrets_bot_token_and_api_key_shapes_in_all_outputs() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    let store = Arc::new(MemoryStore::new());
    store_bot_token(&*store, TEST_BOT_TOKEN).unwrap();
    let custom_secret = "hunter2-super-secret-db-password";
    store.set("db.password", custom_secret).unwrap();
    let openai_key = "sk-abcdefghijklmnopqrstuvwxyz123456";

    let bot = BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot]));

    let cfg = TelegramConfig::new(store)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let call_id = ToolCallId::new();
    daemon.queue_run_events(
        run_id,
        vec![
            Event::ToolCallRequested(ToolCallRequested {
                run_id,
                call_id,
                tool_name: "shell".into(),
                input: json!({ "cmd": format!("export PASS={custom_secret} KEY={openai_key}") }),
            }),
            Event::ToolCallCompleted(ToolCallCompleted {
                run_id,
                call_id,
                error: None,
                output_preview: Some(format!(
                    "output had {custom_secret} and {openai_key} and {TEST_BOT_TOKEN}"
                )),
                duration_ms: Some(12),
            }),
            Event::MessageDelta(MessageDelta {
                run_id,
                text: format!("Result using {custom_secret} and {openai_key}"),
            }),
            Event::RunFinished(RunFinished {
                run_id,
                error: None,
            }),
        ],
    );

    let u: Update =
        serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "run check")).unwrap();
    channel.handle_update(u).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let sends = sent_messages(&reqs);
    assert_eq!(sends.len(), 3);

    for m in &sends {
        let text = m["text"].as_str().unwrap();
        assert!(
            !text.contains(custom_secret),
            "custom secret leaked in: {text}"
        );
        assert!(
            !text.contains(openai_key),
            "openai key pattern leaked in: {text}"
        );
        assert!(
            !text.contains(TEST_BOT_TOKEN),
            "bot token leaked in: {text}"
        );
        assert!(text.contains(REDACTED), "expected {REDACTED} in: {text}");
    }
}

#[tokio::test]
async fn start_polls_get_updates_and_channel_trait_works() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;

    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": [
                text_update(10, PAIRED_USER_ID, TEST_CHAT_ID, "/status")
            ]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": []
        })))
        .mount(&server)
        .await;

    let daemon = Arc::new(FakeDaemon::new(vec![BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    }]));

    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_poll_timeout_secs(0)
        .with_poll_backoff(Duration::from_millis(20))
        .with_paired_users([PAIRED_USER_ID]);

    // Test Channel trait methods directly first.
    let ch = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    assert_eq!(ch.capabilities(), ChannelCaps::telegram_v0());
    let mut rx = ch.receive().await.unwrap();
    let first = rx.next().await.expect("one update");
    assert_eq!(first.chat, ChatRef(TEST_CHAT_ID));
    assert_eq!(first.kind, IncomingKind::Text("/status".into()));

    let msg_id = ch
        .send(
            ChatRef(TEST_CHAT_ID),
            OutgoingMessage::text("hello from trait"),
        )
        .await
        .unwrap();
    assert_eq!(msg_id, 101);

    // Also test `TelegramChannel::start` background loop + graceful shutdown.
    let running = TelegramChannel::start(cfg, daemon).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    running.shutdown().await;
}

#[tokio::test]
async fn http_daemon_client_maps_routes_and_sse_stream() {
    let daemon_srv = MockServer::start().await;
    let bot_id = BotId::new();
    let chat_id = ChatId::new();
    let run_id = RunId::new();

    Mock::given(method("GET"))
        .and(path("/v1/bots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": bot_id.0, "name": "Mira" }
        ])))
        .mount(&daemon_srv)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chats"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": chat_id.0,
            "bot_id": bot_id.0,
            "title": "telegram:7001"
        })))
        .mount(&daemon_srv)
        .await;

    Mock::given(method("POST"))
        .and(path(format!("/v1/chats/{}/messages", chat_id.0)))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "delivery": "new_turn",
            "run_id": run_id.0
        })))
        .mount(&daemon_srv)
        .await;

    let env1 = Envelope::new(
        0,
        Event::MessageDelta(MessageDelta {
            run_id,
            text: "pong".into(),
        }),
    );
    let env2 = Envelope::new(
        1,
        Event::RunFinished(RunFinished {
            run_id,
            error: None,
        }),
    );
    let sse_body = format!(
        "id: 0\nevent: message_delta\ndata: {}\n\nid: 1\nevent: run_finished\ndata: {}\n\n",
        serde_json::to_string(&env1).unwrap(),
        serde_json::to_string(&env2).unwrap()
    );
    Mock::given(method("GET"))
        .and(path("/v1/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&daemon_srv)
        .await;

    let client = HttpDaemonClient::new(daemon_srv.uri(), SecretValue::new("daemon-token"));
    let bots = client.list_bots().await.unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].name, "Mira");

    let chat = client
        .create_chat(bot_id, Some("telegram:7001"))
        .await
        .unwrap();
    assert_eq!(chat.id, chat_id);

    let outcome = client.send_message(chat_id, "ping").await.unwrap();
    assert_eq!(outcome.run_id, Some(run_id));

    let events: Vec<Envelope> = client.subscribe_run(run_id).await.unwrap().collect().await;
    assert_eq!(events.len(), 2);
}
