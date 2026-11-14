//! Hermetic tests for `channel-telegram` using a local `wiremock` Telegram Bot API server.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    ThrottledEditor, Update, format_approval_callback, format_question_callback, store_bot_token,
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
    /// When set, the next `send_message` answers `Queued` with no run id.
    queue_next_message: Mutex<bool>,
    /// Runs `find_chat_run` hands out (the turns the daemon "started later").
    started_later: Mutex<VecDeque<RunId>>,
    send_failures: Mutex<HashMap<String, VecDeque<crate::TelegramError>>>,
    send_attempts: Mutex<Vec<String>>,
    idempotency_keys: Mutex<Vec<String>>,
    subscribe_failures: Mutex<usize>,
    cancel_seen: tokio::sync::Notify,
    cancel_failures: Mutex<VecDeque<crate::TelegramError>>,
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
            queue_next_message: Mutex::new(false),
            started_later: Mutex::new(VecDeque::new()),
            send_failures: Mutex::new(HashMap::new()),
            send_attempts: Mutex::new(Vec::new()),
            idempotency_keys: Mutex::new(Vec::new()),
            subscribe_failures: Mutex::new(0),
            cancel_seen: tokio::sync::Notify::new(),
            cancel_failures: Mutex::new(VecDeque::new()),
        }
    }

    /// Stream fed by the returned sender; stays open until the sender is dropped.
    fn queue_open_run(&self, run_id: RunId) -> mpsc::UnboundedSender<Envelope> {
        let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
        self.queue_run_stream(
            run_id,
            Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|i| (i, rx))
            })),
        );
        tx
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
        self.send_attempts.lock().push(text.to_owned());
        if let Some(error) = self
            .send_failures
            .lock()
            .get_mut(text)
            .and_then(VecDeque::pop_front)
        {
            return Err(error);
        }
        self.sent_messages.lock().push((chat_id, text.to_owned()));
        if std::mem::take(&mut *self.queue_next_message.lock()) {
            return Ok(SendOutcome {
                delivery: DeliveryKind::Queued,
                run_id: None,
            });
        }
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

    async fn send_message_idempotent(
        &self,
        chat_id: ChatId,
        text: &str,
        key: &str,
    ) -> Result<SendOutcome> {
        self.idempotency_keys.lock().push(key.to_owned());
        self.send_message(chat_id, text).await
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
        if let Some(error) = self.cancel_failures.lock().pop_front() {
            return Err(error);
        }
        self.cancelled_runs.lock().push(run_id);
        self.cancel_seen.notify_one();
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

    async fn find_chat_run(
        &self,
        _chat_id: ChatId,
        _exclude: &[RunId],
        _created_after_ms: i64,
    ) -> Result<Option<RunId>> {
        Ok(self.started_later.lock().pop_front())
    }

    async fn subscribe_run(&self, run_id: RunId) -> Result<BoxStream<'static, Envelope>> {
        {
            let mut failures = self.subscribe_failures.lock();
            if *failures > 0 {
                *failures -= 1;
                return Err(crate::TelegramError::Daemon(
                    "injected subscription failure".into(),
                ));
            }
        }
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
    // While streaming only the redaction-safe prefix is shown ("Hello from "); the complete
    // text arrives with the final edit when the run finishes.
    let edits = edited_messages(&reqs);
    assert!(
        msgs.iter()
            .chain(edits.iter())
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
    let call_ids = [ToolCallId::new(), ToolCallId::new(), ToolCallId::new()];

    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    for (i, call_id) in call_ids.iter().enumerate() {
        tx.send(Envelope::new(
            i as u64,
            Event::ApprovalNeeded(ApprovalNeeded {
                run_id,
                call_id: *call_id,
                tool_name: "fs_delete".into(),
                input: json!({ "path": "/tmp/a.txt" }),
                summary: "Delete /tmp/a.txt".into(),
                details: None,
            }),
        ))
        .unwrap();
    }

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
    assert_eq!(msgs.len(), 3);
    let keyboard = &msgs[0]["reply_markup"]["inline_keyboard"][0];
    assert_eq!(keyboard[0]["text"], "Approve once");
    assert_eq!(keyboard[1]["text"], "Always");
    assert_eq!(keyboard[2]["text"], "Deny");

    // msg0 -> "Approve once", msg1 -> "Always", msg2 -> "Deny".
    for (i, qid) in ["cb-1", "cb-2", "cb-3"].into_iter().enumerate() {
        let data = msgs[i]["reply_markup"]["inline_keyboard"][0][i]["callback_data"]
            .as_str()
            .unwrap();
        let cb: Update = serde_json::from_value(callback_update(
            i64::try_from(i + 2).unwrap(),
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
                call_id: call_ids[0],
                decision: ApprovalDecision::Approve,
                scope: ApprovalScope::Once,
            },
            RecordedApproval {
                call_id: call_ids[1],
                decision: ApprovalDecision::Approve,
                scope: ApprovalScope::Always,
            },
            RecordedApproval {
                call_id: call_ids[2],
                decision: ApprovalDecision::Deny,
                scope: ApprovalScope::Once,
            },
        ]
    );
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(callback_answers(&reqs).len(), 3);

    // Single use: replaying a button must not reach the daemon again.
    let replay: Update = serde_json::from_value(callback_update(
        9,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "cb-replay",
        &format_approval_callback(call_ids[0], "once"),
    ))
    .unwrap();
    channel.handle_update(replay).await.unwrap();
    assert_eq!(daemon.approvals.lock().len(), 3);
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

// ---------------------------------------------------------------------------
// Security review additions
// ---------------------------------------------------------------------------

const OTHER_PAIRED_USER_ID: i64 = 4343;
const OTHER_CHAT_ID: i64 = 7002;

fn one_bot_daemon() -> Arc<FakeDaemon> {
    Arc::new(FakeDaemon::new(vec![BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    }]))
}

fn update(v: Value) -> Update {
    serde_json::from_value(v).unwrap()
}

fn with_chat_type(mut v: Value, kind: &str) -> Value {
    if v.get("message").is_some() {
        v["message"]["chat"]["type"] = json!(kind);
    } else {
        v["callback_query"]["message"]["chat"]["type"] = json!(kind);
    }
    v
}

#[tokio::test]
async fn non_private_chats_and_other_update_types_are_ignored() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    for kind in ["group", "supergroup", "channel"] {
        // Even a paired user is ignored in non-private chats (also /pair and callbacks).
        let msg = with_chat_type(text_update(1, PAIRED_USER_ID, -100, "hello"), kind);
        assert!(update(msg.clone()).into_incoming().is_none());
        channel.handle_update(update(msg)).await.unwrap();
        let code = channel.create_pair_code();
        let pair = with_chat_type(
            text_update(2, UNPAIRED_USER_ID, -100, &format!("/pair {code}")),
            kind,
        );
        channel.handle_update(update(pair)).await.unwrap();
        let cb = with_chat_type(
            callback_update(3, PAIRED_USER_ID, -100, "q", "approve:once:x"),
            kind,
        );
        channel.handle_update(update(cb)).await.unwrap();
    }
    assert!(!channel.is_paired(UNPAIRED_USER_ID));

    // Messages from bots, messages without a chat type, and edited/channel posts are dropped.
    let mut from_bot = text_update(4, PAIRED_USER_ID, TEST_CHAT_ID, "hi");
    from_bot["message"]["from"]["is_bot"] = json!(true);
    assert!(update(from_bot).into_incoming().is_none());
    let mut no_type = text_update(5, PAIRED_USER_ID, TEST_CHAT_ID, "hi");
    no_type["message"]["chat"]
        .as_object_mut()
        .unwrap()
        .remove("type");
    assert!(update(no_type).into_incoming().is_none());
    let edited = json!({
        "update_id": 6,
        "edited_message": text_update(6, PAIRED_USER_ID, TEST_CHAT_ID, "hi")["message"].clone(),
    });
    assert!(update(edited).into_incoming().is_none());
    let channel_post = json!({
        "update_id": 7,
        "channel_post": text_update(7, PAIRED_USER_ID, TEST_CHAT_ID, "hi")["message"].clone(),
    });
    assert!(update(channel_post).into_incoming().is_none());

    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(daemon.sent_messages.lock().is_empty());
}

#[tokio::test]
async fn forged_approval_callbacks_are_rejected() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID, OTHER_PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let call_id = ToolCallId::new();
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::ApprovalNeeded(ApprovalNeeded {
            run_id,
            call_id,
            tool_name: "shell".into(),
            input: json!({}),
            summary: "rm".into(),
            details: None,
        }),
    ))
    .unwrap();
    daemon.queue_run_stream(
        run_id,
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|i| (i, rx))
        })),
    );
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")))
        .await
        .unwrap();

    // 1. Unknown call id from a paired user.
    let unknown = format_approval_callback(ToolCallId::new(), "once");
    channel
        .handle_update(update(callback_update(
            2,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "c1",
            &unknown,
        )))
        .await
        .unwrap();
    // 2. Real call id, but pressed from another paired user's chat.
    let real = format_approval_callback(call_id, "always");
    channel
        .handle_update(update(callback_update(
            3,
            OTHER_PAIRED_USER_ID,
            OTHER_CHAT_ID,
            "c2",
            &real,
        )))
        .await
        .unwrap();
    // 3. Real call id from an unpaired user in the right chat.
    channel
        .handle_update(update(callback_update(
            4,
            UNPAIRED_USER_ID,
            TEST_CHAT_ID,
            "c3",
            &real,
        )))
        .await
        .unwrap();
    assert!(
        daemon.approvals.lock().is_empty(),
        "forged callbacks must not decide"
    );

    // 4. The legitimate press still works.
    channel
        .handle_update(update(callback_update(
            5,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "c4",
            &real,
        )))
        .await
        .unwrap();
    assert_eq!(daemon.approvals.lock().len(), 1);
}

#[tokio::test]
async fn forged_question_callbacks_are_rejected() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID, OTHER_PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let call_id = ToolCallId::new();
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::QuestionAsked(QuestionAsked {
            run_id,
            call_id,
            question: "Which?".into(),
            options: vec!["A".into(), "B".into()],
        }),
    ))
    .unwrap();
    daemon.queue_run_stream(
        run_id,
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|i| (i, rx))
        })),
    );
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")))
        .await
        .unwrap();

    let forged = [
        // unknown call id
        (
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            format_question_callback(ToolCallId::new(), 0),
        ),
        // other chat
        (
            OTHER_PAIRED_USER_ID,
            OTHER_CHAT_ID,
            format_question_callback(call_id, 0),
        ),
        // arbitrary payload instead of an offered option index
        (
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            format!("question:{}:rm -rf /", call_id.0),
        ),
        // out-of-range option index
        (
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            format_question_callback(call_id, 9),
        ),
    ];
    for (i, (user, chat, data)) in forged.iter().enumerate() {
        channel
            .handle_update(update(callback_update(
                i64::try_from(i + 2).unwrap(),
                *user,
                *chat,
                "cq",
                data,
            )))
            .await
            .unwrap();
    }
    assert!(daemon.answers.lock().is_empty());

    channel
        .handle_update(update(callback_update(
            20,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "cq",
            &format_question_callback(call_id, 1),
        )))
        .await
        .unwrap();
    let answers = daemon.answers.lock().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answer, "B");
}

#[test]
fn pairing_codes_are_random_six_digits_and_user_lockout_applies() {
    use crate::pairing::{MAX_PAIR_ATTEMPTS_PER_USER, PairAttempt, PairingStore};
    let store = PairingStore::in_memory();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let c = store.create_code();
        assert_eq!(c.len(), 6);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
        seen.insert(c);
    }
    assert!(
        seen.len() > 150,
        "codes must be random, got {} unique",
        seen.len()
    );

    // A single user is locked out after the per-user cap, even for the right code.
    let store = PairingStore::in_memory();
    let code = store.create_code();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..MAX_PAIR_ATTEMPTS_PER_USER {
        let _ = store.redeem_code(1, wrong);
    }
    let live = store.create_code();
    assert_eq!(
        store.redeem_code(1, &live).unwrap(),
        PairAttempt::RateLimited
    );
    assert!(!store.is_paired(1));
    // Another user is unaffected.
    assert_eq!(store.redeem_code(2, &live).unwrap(), PairAttempt::Paired);
    assert_eq!(
        store.redeem_code(3, &live).unwrap(),
        PairAttempt::InvalidCode
    );
}

#[test]
fn global_wrong_guess_cap_burns_pending_codes() {
    use crate::pairing::{MAX_PAIR_ATTEMPTS, PairAttempt, PairingStore};
    let store = PairingStore::in_memory();
    let code = store.create_code();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    // Different users each guess once: the shared cap still burns the code.
    for uid in 0..i64::from(MAX_PAIR_ATTEMPTS) {
        assert_eq!(
            store.redeem_code(100 + uid, wrong).unwrap(),
            PairAttempt::InvalidCode
        );
    }
    assert_eq!(
        store.redeem_code(999, &code).unwrap(),
        PairAttempt::InvalidCode
    );
    assert!(!store.is_paired(999));
}

#[test]
fn refusals_are_globally_rate_limited_and_per_user_once() {
    use crate::pairing::{MAX_REFUSALS_PER_MINUTE, PairingStore};
    let store = PairingStore::in_memory();
    let sent = (0..(i64::try_from(MAX_REFUSALS_PER_MINUTE).unwrap() + 20))
        .filter(|uid| store.should_send_refusal(*uid))
        .count();
    assert_eq!(sent, MAX_REFUSALS_PER_MINUTE);
    assert!(
        !store.should_send_refusal(0),
        "same user is refused only once"
    );
}

#[tokio::test]
async fn transport_errors_never_contain_the_bot_token() {
    // Nothing listens on port 1: reqwest's error text would normally embed the URL.
    let client =
        crate::BotApiClient::with_base_url(SecretValue::new(TEST_BOT_TOKEN), "http://127.0.0.1:1");
    let err = client.send_message(1, "x", None).await.unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains(TEST_BOT_TOKEN), "token leaked: {text}");
    assert!(!text.contains("AAH_test_bot_token"), "token leaked: {text}");
    assert!(!format!("{client:?}").contains("AAH_test_bot_token"));
}

#[tokio::test]
async fn api_errors_echoing_the_token_are_redacted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "ok": false,
            "error_code": 400,
            "description": format!("bad request for {TEST_BOT_TOKEN}"),
        })))
        .mount(&server)
        .await;
    let client = crate::BotApiClient::with_base_url(SecretValue::new(TEST_BOT_TOKEN), server.uri());
    let err = client.send_message(1, "x", None).await.unwrap_err();
    assert!(!err.to_string().contains(TEST_BOT_TOKEN));
}

#[tokio::test]
async fn rate_limit_429_is_retried_for_sends_and_surfaced_for_polling() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests: retry after 1",
            "parameters": { "retry_after": 1 }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": { "message_id": 5, "chat": { "id": 1, "type": "private" }, "text": "x" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests",
            "parameters": { "retry_after": 7 }
        })))
        .mount(&server)
        .await;
    let client = crate::BotApiClient::with_base_url(SecretValue::new(TEST_BOT_TOKEN), server.uri());
    let started = std::time::Instant::now();
    let msg = client.send_message(1, "x", None).await.unwrap();
    assert_eq!(msg.message_id, 5);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "must honour retry_after"
    );

    let err = client.get_updates(None, 0).await.unwrap_err();
    assert!(matches!(
        err,
        crate::TelegramError::RateLimited {
            retry_after_secs: 7
        }
    ));
}

#[test]
fn poll_error_backoff_policy() {
    use crate::TelegramError;
    use crate::bridge::poll_error_delay;
    let base = Duration::from_millis(100);
    let limited = TelegramError::RateLimited {
        retry_after_secs: 7,
    };
    assert_eq!(poll_error_delay(&limited, 1, base), Duration::from_secs(7));
    let other = TelegramError::Http("boom".into());
    assert_eq!(poll_error_delay(&other, 1, base), base);
    assert_eq!(poll_error_delay(&other, 3, base), base * 4);
    assert_eq!(poll_error_delay(&other, 60, base), Duration::from_secs(30));
}

#[tokio::test]
async fn polling_offset_is_persisted_across_restarts() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": [
                text_update(10, UNPAIRED_USER_ID, TEST_CHAT_ID, "hi"),
                text_update(11, PAIRED_USER_ID, TEST_CHAT_ID, "/status")
            ]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": [] })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let make = || {
        TelegramChannel::new(
            TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
                .with_api_base_url(server.uri())
                .with_poll_timeout_secs(0)
                .with_state_path(&state)
                .with_paired_users([PAIRED_USER_ID]),
            one_bot_daemon(),
        )
        .unwrap()
    };

    let first = make();
    assert_eq!(first.poll_once().await.unwrap(), 2);
    drop(first);

    let second = make();
    second.poll_once().await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    let polls: Vec<Value> = reqs
        .iter()
        .filter(|r| r.url.path() == tg_endpoint("getUpdates"))
        .filter_map(|r| serde_json::from_slice(&r.body).ok())
        .collect();
    assert_eq!(polls.len(), 2);
    assert!(polls[0].get("offset").is_none());
    assert_eq!(
        polls[1]["offset"], 12,
        "restart must resume after the last update"
    );
    assert_eq!(
        polls[1]["allowed_updates"],
        json!(["message", "callback_query"])
    );
}

#[tokio::test]
async fn a_failing_update_does_not_starve_the_rest_of_the_batch() {
    let server = MockServer::start().await;
    // sendMessage always fails (e.g. the unpaired user blocked the bot).
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "ok": false, "error_code": 403, "description": "Forbidden: bot was blocked by the user"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": [
                text_update(1, UNPAIRED_USER_ID, TEST_CHAT_ID, "hi"),
                text_update(2, PAIRED_USER_ID, TEST_CHAT_ID, "do the thing")
            ]
        })))
        .mount(&server)
        .await;
    let daemon = one_bot_daemon();
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(server.uri())
            .with_poll_timeout_secs(0)
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();
    let _ = channel.poll_once().await;
    assert_eq!(
        daemon.sent_messages.lock().len(),
        1,
        "the paired user's message must still be processed"
    );
}

#[test]
fn plain_http_to_non_loopback_hosts_is_rejected() {
    for bad in [
        "http://api.telegram.org",
        "ftp://x",
        "http://127.0.0.1.evil.com",
    ] {
        let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN).with_api_base_url(bad);
        assert!(
            TelegramChannel::new(cfg, one_bot_daemon()).is_err(),
            "{bad} must be rejected"
        );
    }
    for good in [
        "https://api.telegram.org",
        "http://127.0.0.1:8080",
        "http://localhost:9",
    ] {
        let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN).with_api_base_url(good);
        assert!(
            TelegramChannel::new(cfg, one_bot_daemon()).is_ok(),
            "{good}"
        );
    }
}

#[tokio::test]
async fn secrets_cut_by_truncation_are_still_redacted() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let store = Arc::new(MemoryStore::new());
    store_bot_token(&*store, TEST_BOT_TOKEN).unwrap();
    let secret = "hunter2-super-secret-db-password";
    store.set("db.password", secret).unwrap();
    let daemon = one_bot_daemon();
    let channel = TelegramChannel::new(
        TelegramConfig::new(store)
            .with_api_base_url(server.uri())
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();

    let run_id = RunId::new();
    let call_id = ToolCallId::new();
    // Place the secret so that the 160/200-char tool-line limits would cut it in half.
    let padding = "x".repeat(145);
    let output_padding = "y".repeat(185);
    daemon.queue_run_events(
        run_id,
        vec![
            Event::ToolCallRequested(ToolCallRequested {
                run_id,
                call_id,
                tool_name: "shell".into(),
                input: json!({ "cmd": format!("{padding}{secret}") }),
            }),
            Event::ToolCallCompleted(ToolCallCompleted {
                run_id,
                call_id,
                error: None,
                output_preview: Some(format!("{output_padding} {secret}")),
                duration_ms: None,
            }),
            Event::RunFinished(RunFinished {
                run_id,
                error: None,
            }),
        ],
    );
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let sends = sent_messages(&reqs);
    assert!(sends.len() >= 2);
    for m in sends {
        let text = m["text"].as_str().unwrap();
        assert!(!text.contains("hunter2"), "partial secret leaked: {text}");
    }
}

#[tokio::test]
async fn oversized_incoming_text_is_not_forwarded() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(server.uri())
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();
    let huge = "a".repeat(100_000);
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, &huge)))
        .await
        .unwrap();
    assert!(daemon.sent_messages.lock().is_empty());
}

// ---------------------------------------------------------------------------
// F04 fix wave
// ---------------------------------------------------------------------------

fn run_finished(run_id: RunId) -> Envelope {
    Envelope::new(
        99,
        Event::RunFinished(RunFinished {
            run_id,
            error: None,
        }),
    )
}

fn sent_texts(reqs: &[Request]) -> Vec<String> {
    sent_messages(reqs)
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn sse_buffer_survives_multibyte_and_crlf_split_across_chunks() {
    use crate::daemon_client::SseBuffer;
    let block = "data: {\"t\":\"şğı Türkçe ✓\"}\n\n";
    let bytes = block.as_bytes();
    // Split at every possible byte offset: the decoded block must be identical.
    for cut in 1..bytes.len() {
        let mut buf = SseBuffer::default();
        buf.push(&bytes[..cut]);
        let early = buf.next_block();
        buf.push(&bytes[cut..]);
        let got = early.or_else(|| buf.next_block()).expect("block");
        assert_eq!(got, block, "cut at {cut}");
        assert!(buf.next_block().is_none());
    }
    // CRLF terminators, with the CR LF pair split across chunks.
    let crlf = "data: a\r\n\r\ndata: b\r\n\r\n";
    let bytes = crlf.as_bytes();
    for cut in 1..bytes.len() {
        let mut buf = SseBuffer::default();
        buf.push(&bytes[..cut]);
        buf.push(&bytes[cut..]);
        let first = buf.next_block().expect("first");
        let second = buf.next_block().expect("second");
        assert_eq!(first, "data: a\r\n\r\n");
        assert_eq!(second, "data: b\r\n\r\n");
    }
}

#[test]
fn safe_stream_prefix_withholds_unfinished_trailing_token() {
    use crate::bridge::safe_stream_prefix;
    assert_eq!(safe_stream_prefix("key sk-abc"), "key ");
    assert_eq!(safe_stream_prefix("key sk-abc "), "key sk-abc ");
    assert_eq!(safe_stream_prefix("no-whitespace-yet"), "");
    assert_eq!(safe_stream_prefix("ş ğ"), "ş ");
    assert_eq!(safe_stream_prefix(""), "");
}

#[tokio::test]
async fn streaming_never_shows_a_secret_prefix_in_intermediate_edits() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_edit_interval(Duration::from_millis(1))
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_id = RunId::new();
    let tx = daemon.queue_open_run(run_id);
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")))
        .await
        .unwrap();

    let delta = |seq: u64, text: &str| {
        Envelope::new(
            seq,
            Event::MessageDelta(MessageDelta {
                run_id,
                text: text.into(),
            }),
        )
    };
    // The key is 36 chars; the first deltas carry only a prefix that no pattern matches.
    tx.send(delta(0, "Your key is sk-abcdefghijkl")).unwrap();
    channel
        .handle_update(update(text_update(
            2,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/status",
        )))
        .await
        .unwrap();
    tx.send(delta(1, "mnopqrstuvwxyz0123456789 ok")).unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    channel
        .handle_update(update(text_update(
            3,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/status",
        )))
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    for t in sent_texts(&reqs).into_iter().chain(
        edited_messages(&reqs)
            .iter()
            .filter_map(|m| m["text"].as_str().map(str::to_owned)),
    ) {
        assert!(
            !t.contains("sk-abc"),
            "secret prefix leaked mid-stream: {t}"
        );
    }

    tx.send(run_finished(run_id)).unwrap();
    channel
        .handle_update(update(text_update(
            4,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/status",
        )))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let last_edit = edited_messages(&reqs)
        .last()
        .and_then(|m| m["text"].as_str().map(str::to_owned))
        .expect("final edit");
    assert!(last_edit.contains(REDACTED), "final text: {last_edit}");
    assert!(last_edit.contains("ok"), "final text complete: {last_edit}");
    assert!(!last_edit.contains("sk-abc"), "final text: {last_edit}");
}

#[tokio::test]
async fn one_runs_telegram_failure_does_not_skip_cleanup_of_finished_runs() {
    use wiremock::matchers::body_partial_json;
    let server = MockServer::start().await;
    // Chat 2 is a user who blocked the bot: every send there fails with 403.
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .and(body_partial_json(json!({ "chat_id": OTHER_CHAT_ID })))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "ok": false, "error_code": 403,
            "description": "Forbidden: bot was blocked by the user"
        })))
        .mount(&server)
        .await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID, OTHER_PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_a = RunId::new();
    let tx_a = daemon.queue_open_run(run_a);
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "a")))
        .await
        .unwrap();

    let run_b = RunId::new();
    let tx_b = daemon.queue_open_run(run_b);
    let _ = channel
        .handle_update(update(text_update(
            2,
            OTHER_PAIRED_USER_ID,
            OTHER_CHAT_ID,
            "b",
        )))
        .await;

    // A finishes; B's next event cannot be delivered (403).
    tx_a.send(run_finished(run_a)).unwrap();
    tx_b.send(Envelope::new(
        5,
        Event::ToolCallRequested(ToolCallRequested {
            run_id: run_b,
            call_id: ToolCallId::new(),
            tool_name: "shell".into(),
            input: json!({}),
        }),
    ))
    .unwrap();
    let blocked = channel
        .handle_update(update(text_update(
            3,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/status",
        )))
        .await;
    assert!(blocked.is_err(), "B's 403 is still reported");

    // A was cleaned up in spite of the error, and polling further does not panic.
    for i in 4..7 {
        let _ = channel
            .handle_update(update(text_update(
                i,
                PAIRED_USER_ID,
                TEST_CHAT_ID,
                "/status",
            )))
            .await;
    }
    let reqs = server.received_requests().await.unwrap();
    let texts = sent_texts(&reqs);
    let last_status = texts
        .iter()
        .rev()
        .find(|t| t.starts_with("Daemon:"))
        .expect("status reply");
    assert!(last_status.contains("Run: idle"), "{last_status}");
}

#[tokio::test]
async fn message_queued_behind_a_run_is_followed_once_it_starts() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let run_a = RunId::new();
    let tx_a = daemon.queue_open_run(run_a);
    channel
        .handle_update(update(text_update(
            1,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "first",
        )))
        .await
        .unwrap();

    *daemon.queue_next_message.lock() = true;
    channel
        .handle_update(update(text_update(
            2,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "second",
        )))
        .await
        .unwrap();

    // The daemon starts the queued turn as run B after A finished.
    let run_b = RunId::new();
    daemon.queue_run_events(
        run_b,
        vec![
            Event::MessageDelta(MessageDelta {
                run_id: run_b,
                text: "answer to the second message ".into(),
            }),
            Event::RunFinished(RunFinished {
                run_id: run_b,
                error: None,
            }),
        ],
    );
    daemon.started_later.lock().push_back(run_b);
    tx_a.send(run_finished(run_a)).unwrap();
    for i in 3..8 {
        channel
            .handle_update(update(text_update(
                i,
                PAIRED_USER_ID,
                TEST_CHAT_ID,
                "/status",
            )))
            .await
            .unwrap();
    }
    let reqs = server.received_requests().await.unwrap();
    let all: Vec<String> = sent_texts(&reqs)
        .into_iter()
        .chain(
            edited_messages(&reqs)
                .iter()
                .filter_map(|m| m["text"].as_str().map(str::to_owned)),
        )
        .collect();
    assert!(
        all.iter()
            .any(|t| t.contains("answer to the second message")),
        "queued turn output must reach Telegram: {all:?}"
    );
}

#[tokio::test]
async fn subagent_approvals_and_questions_are_shown_in_the_parent_chat() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();

    let parent = RunId::new();
    let child = RunId::new();
    let child_call = ToolCallId::new();
    let tx_parent = daemon.queue_open_run(parent);
    let tx_child = daemon.queue_open_run(child);
    tx_parent
        .send(Envelope::new(
            0,
            Event::SubagentStarted(protocol::event::SubagentStarted {
                run_id: child,
                parent_run_id: parent,
                task: "dig".into(),
                call_id: None,
            }),
        ))
        .unwrap();
    tx_child
        .send(Envelope::new(
            0,
            Event::ApprovalNeeded(ApprovalNeeded {
                run_id: child,
                call_id: child_call,
                tool_name: "shell".into(),
                input: json!({}),
                summary: "child wants rm".into(),
                details: None,
            }),
        ))
        .unwrap();
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")))
        .await
        .unwrap();
    for i in 2..5 {
        channel
            .handle_update(update(text_update(
                i,
                PAIRED_USER_ID,
                TEST_CHAT_ID,
                "/status",
            )))
            .await
            .unwrap();
    }
    let reqs = server.received_requests().await.unwrap();
    let approval = sent_messages(&reqs)
        .into_iter()
        .find(|m| {
            m["text"]
                .as_str()
                .is_some_and(|t| t.contains("child wants rm"))
        })
        .expect("child approval must be shown");
    assert!(approval["reply_markup"].is_object(), "buttons attached");

    // The button works: it is bound to the parent's Telegram chat.
    channel
        .handle_update(update(callback_update(
            9,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "q-child",
            &format_approval_callback(child_call, "once"),
        )))
        .await
        .unwrap();
    assert_eq!(daemon.approvals.lock().len(), 1);
}

#[tokio::test]
async fn active_runs_do_not_shorten_the_get_updates_long_poll() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": [ text_update(10, PAIRED_USER_ID, TEST_CHAT_ID, "go") ]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(150))
                .set_body_json(json!({ "ok": true, "result": [] })),
        )
        .mount(&server)
        .await;

    let daemon = one_bot_daemon();
    let _tx = daemon.queue_open_run(RunId::new());
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_poll_timeout_secs(7)
        .with_poll_backoff(Duration::from_millis(10))
        .with_paired_users([PAIRED_USER_ID]);
    let running = TelegramChannel::start(cfg, daemon).unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    running.shutdown().await;

    let reqs = server.received_requests().await.unwrap();
    let target = tg_endpoint("getUpdates");
    let timeouts: Vec<i64> = reqs
        .iter()
        .filter(|r| r.url.path() == target)
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|b| b["timeout"].as_i64())
        .collect();
    assert!(timeouts.len() >= 2, "polled: {timeouts:?}");
    assert!(
        timeouts.iter().all(|&t| t == 7),
        "getUpdates must keep the long-poll timeout while a run is active: {timeouts:?}"
    );
}

#[test]
fn a_new_pair_code_invalidates_older_live_codes() {
    use crate::pairing::{PairAttempt, PairingStore};
    let store = PairingStore::in_memory();
    let mut old = Vec::new();
    for _ in 0..20 {
        old.push(store.create_code());
    }
    let newest = store.create_code();
    // Only the newest code is live; the shared guess budget protects a single code.
    for c in &old {
        if *c == newest {
            continue;
        }
        let uid = 500 + i64::from(c.as_bytes()[0]);
        assert_ne!(
            store.redeem_code(uid, c).unwrap(),
            PairAttempt::Paired,
            "older code {c} must be invalid"
        );
        assert!(!store.is_paired(uid));
    }
    // 5 distinct wrong guesses above burned `newest` only if they reached the global cap;
    // use a fresh store for the positive path.
    let store = PairingStore::in_memory();
    let _older = store.create_code();
    let newest = store.create_code();
    assert_eq!(store.redeem_code(1, &newest).unwrap(), PairAttempt::Paired);
}

#[tokio::test]
async fn http_find_chat_run_accepts_queued_and_ended_runs_of_the_chat() {
    use uuid::Uuid;
    let server = MockServer::start().await;
    let chat = ChatId::new();
    let other_chat = ChatId::new();
    let mk = |id: Uuid, chat: ChatId, state: &str, created: i64, parent: Option<Uuid>| {
        json!({
            "id": id.to_string(), "bot_id": Uuid::new_v4().to_string(),
            "chat_id": chat.0.to_string(), "kind": "turn", "state": state,
            "created_at_ms": created, "updated_at_ms": created,
            "parent_run_id": parent.map(|p| p.to_string()),
        })
    };
    let (old, queued, ended, other, child, attached) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    Mock::given(method("GET"))
        .and(path("/v1/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            mk(old, chat, "running", 50, None),
            mk(attached, chat, "running", 150, None),
            mk(child, chat, "queued", 160, Some(attached)),
            mk(other, other_chat, "queued", 170, None),
            mk(ended, chat, "succeeded", 300, None),
            mk(queued, chat, "queued", 400, None),
        ])))
        .mount(&server)
        .await;
    let client = HttpDaemonClient::new(server.uri(), SecretValue::new("tok"));

    // The daemon stores `queued` (never `pending`): a just-started queued run is found.
    let found = client
        .find_chat_run(chat, &[RunId(attached), RunId(ended)], 100)
        .await
        .unwrap();
    assert_eq!(found, Some(RunId(queued)));
    // A run that already ended (subscribe replays its log) is found too, oldest first;
    // older runs, subagent children, other chats and attached runs are not.
    let found = client
        .find_chat_run(chat, &[RunId(attached)], 100)
        .await
        .unwrap();
    assert_eq!(found, Some(RunId(ended)));
    assert_eq!(client.find_chat_run(chat, &[], 1000).await.unwrap(), None);
}

#[tokio::test]
async fn run_loop_keeps_draining_queued_turns_while_get_updates_blocks() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let upd = |id: i64, text: &str| json!({ "ok": true, "result": [ text_update(id, PAIRED_USER_ID, TEST_CHAT_ID, text) ] });
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(upd(1, "first")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(upd(2, "second")),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // From then on the long poll blocks far longer than the test runs.
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(20))
                .set_body_json(json!({ "ok": true, "result": [] })),
        )
        .mount(&server)
        .await;

    let daemon = one_bot_daemon();
    let run_a = RunId::new();
    let tx_a = daemon.queue_open_run(run_a);
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_poll_timeout_secs(25)
        .with_poll_backoff(Duration::from_millis(10))
        .with_paired_users([PAIRED_USER_ID]);
    let running = TelegramChannel::start(cfg, daemon.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    *daemon.queue_next_message.lock() = true;

    // A finishes before the daemon created the queued run: the first lookups find nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tx_a.send(run_finished(run_a)).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let run_b = RunId::new();
    daemon.queue_run_events(
        run_b,
        vec![
            Event::MessageDelta(MessageDelta {
                run_id: run_b,
                text: "late queued answer ".into(),
            }),
            Event::RunFinished(RunFinished {
                run_id: run_b,
                error: None,
            }),
        ],
    );
    daemon.started_later.lock().push_back(run_b);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    running.shutdown().await;

    let reqs = server.received_requests().await.unwrap();
    let all: Vec<String> = sent_texts(&reqs)
        .into_iter()
        .chain(
            edited_messages(&reqs)
                .iter()
                .filter_map(|m| m["text"].as_str().map(str::to_owned)),
        )
        .collect();
    assert!(
        all.iter().any(|t| t.contains("late queued answer")),
        "queued turn must be found by the drain loop without a new update: {all:?}"
    );
}

#[tokio::test]
async fn a_stream_that_drops_without_run_finished_still_shows_the_last_word() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();
    let run = RunId::new();
    // No RunFinished: the stream just ends.
    let tx = daemon.queue_open_run(run);
    tx.send(Envelope::new(
        0,
        Event::MessageDelta(MessageDelta {
            run_id: run,
            text: "hello wor".into(),
        }),
    ))
    .unwrap();
    channel
        .handle_update(update(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "hi")))
        .await
        .unwrap();
    tx.send(Envelope::new(
        1,
        Event::MessageDelta(MessageDelta {
            run_id: run,
            text: "ld".into(),
        }),
    ))
    .unwrap();
    drop(tx);
    channel
        .handle_update(update(text_update(
            2,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/status",
        )))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let all: Vec<String> = sent_texts(&reqs)
        .into_iter()
        .chain(
            edited_messages(&reqs)
                .iter()
                .filter_map(|m| m["text"].as_str().map(str::to_owned)),
        )
        .collect();
    assert!(
        all.iter().any(|t| t.contains("hello world")),
        "last word must be flushed when the stream ends: {all:?}"
    );
}

// Audit BUG-18: failed persistence must not grant access, consume an invite, or
// publish a different bot/thread/offset. Failure injection is independent of OS ACLs.
#[test]
fn failed_pairing_save_preserves_invite_and_all_live_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telegram_state.json");
    let store = crate::PairingStore::open(&path).unwrap();
    let old_bot = BotId::new();
    let old_chat = ChatId::new();
    store.add_paired_user(1).unwrap();
    store.set_bot_for_chat(1, old_bot).unwrap();
    store.set_thread_for(1, old_bot, old_chat).unwrap();
    store.save_offset("test", 10).unwrap();
    let before = std::fs::read(&path).unwrap();
    let code = store.create_code();
    store.fail_saves(true);
    assert!(store.redeem_code(2, &code).is_err());
    assert!(!store.is_paired(2));
    assert!(store.add_paired_user(3).is_err());
    assert!(!store.is_paired(3));
    assert!(store.set_bot_for_chat(1, BotId::new()).is_err());
    assert_eq!(store.bot_for_chat(1), Some(old_bot));
    assert!(store.set_thread_for(1, old_bot, ChatId::new()).is_err());
    assert_eq!(store.thread_for(1, old_bot), Some(old_chat));
    assert!(store.save_offset("test", 99).is_err());
    assert_eq!(store.saved_offset("test"), Some(10));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    store.fail_saves(false);
    assert_eq!(
        store.redeem_code(2, &code).unwrap(),
        crate::PairAttempt::Paired
    );
    assert!(crate::PairingStore::open(&path).unwrap().is_paired(2));
}

#[test]
fn failed_pairing_rename_cleans_temp_and_keeps_previous_route() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telegram_state.json");
    let store = crate::PairingStore::open(&path).unwrap();
    let old_bot = BotId::new();
    store.set_bot_for_chat(1, old_bot).unwrap();
    let backup = dir.path().join("before.json");
    std::fs::rename(&path, &backup).unwrap();
    // A file cannot replace a directory on Windows or Unix; no root/ACL exception.
    std::fs::create_dir(&path).unwrap();
    assert!(store.set_bot_for_chat(1, BotId::new()).is_err());
    assert_eq!(store.bot_for_chat(1), Some(old_bot));
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|item| {
        !item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    let new_bot = BotId::new();
    store.set_bot_for_chat(1, new_bot).unwrap();
    assert_eq!(
        crate::PairingStore::open(path).unwrap().bot_for_chat(1),
        Some(new_bot)
    );
}

fn inbox_records(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("telegram-inbox.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn mock_updates(server: &MockServer, updates: Value) {
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": updates})),
        )
        .mount(server)
        .await;
}

fn durable_config(server: &MockServer, dir: &std::path::Path) -> TelegramConfig {
    TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_state_path(dir.join("telegram_state.json"))
        .with_paired_users([PAIRED_USER_ID])
        .with_edit_interval(Duration::ZERO)
}

// Audit BUG-17: failure on #2 retains it, blocks #3 until retry, and all duplicate
// batches are deduplicated before the daemon. Time advances via an injected clock.
#[tokio::test]
async fn inbox_retries_in_order_without_redelivering_acknowledged_updates() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([
            text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "one"),
            text_update(2, PAIRED_USER_ID, TEST_CHAT_ID, "two"),
            text_update(3, PAIRED_USER_ID, TEST_CHAT_ID, "three"),
        ]),
    )
    .await;
    let daemon = one_bot_daemon();
    daemon.send_failures.lock().insert(
        "two".into(),
        VecDeque::from([crate::TelegramError::Daemon(
            "temporarily unavailable".into(),
        )]),
    );
    let dir = tempfile::tempdir().unwrap();
    let channel =
        TelegramChannel::new(durable_config(&server, dir.path()), daemon.clone()).unwrap();
    assert!(channel.poll_once().await.is_err());
    assert_eq!(*daemon.send_attempts.lock(), vec!["one", "two"]);
    assert_eq!(channel.pairing().saved_offset("123456789"), Some(4));
    channel.advance_retry_clock(Duration::from_secs(2));
    channel.poll_once().await.unwrap();
    channel.poll_once().await.unwrap();
    assert_eq!(
        daemon
            .sent_messages
            .lock()
            .iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
    assert_eq!(
        *daemon.idempotency_keys.lock(),
        vec!["tg:1", "tg:2", "tg:2", "tg:3"]
    );
    let records = inbox_records(dir.path());
    let first_delivery = records.iter().position(|r| r["delivered"] == true).unwrap();
    assert_eq!(
        records[..first_delivery]
            .iter()
            .filter(|r| r["attempts"] == 0)
            .count(),
        3
    );
    for id in 1..=3 {
        assert_eq!(
            records.iter().rev().find(|r| r["update_id"] == id).unwrap()["state"],
            "done"
        );
    }
}

#[tokio::test]
async fn restart_after_inbox_fsync_replays_pending_once() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([text_update(
            41,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "survives restart"
        )]),
    )
    .await;
    let daemon = one_bot_daemon();
    let dir = tempfile::tempdir().unwrap();
    let cfg = durable_config(&server, dir.path());
    let first = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    // Raw receive persists the batch and offset before handing it to a consumer.
    // Drop that consumer to model a crash before bridge delivery.
    drop(first.receive().await.unwrap());
    assert!(daemon.sent_messages.lock().is_empty());
    assert_eq!(first.pairing().saved_offset("123456789"), Some(42));
    drop(first);
    let restarted = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    restarted.poll_once().await.unwrap();
    drop(restarted);
    let restarted = TelegramChannel::new(cfg, daemon.clone()).unwrap();
    restarted.poll_once().await.unwrap();
    assert_eq!(daemon.sent_messages.lock().len(), 1);
}

#[tokio::test]
async fn persisted_daemon_ack_prevents_resend_after_subscription_failure_and_restart() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([text_update(50, PAIRED_USER_ID, TEST_CHAT_ID, "accepted")]),
    )
    .await;
    let daemon = one_bot_daemon();
    *daemon.subscribe_failures.lock() = 1;
    let dir = tempfile::tempdir().unwrap();
    let cfg = durable_config(&server, dir.path());
    let first = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    assert!(first.poll_once().await.is_err());
    assert_eq!(daemon.sent_messages.lock().len(), 1);
    drop(first);
    let restarted = TelegramChannel::new(cfg, daemon.clone()).unwrap();
    restarted.advance_retry_clock(Duration::from_secs(2));
    restarted.poll_once().await.unwrap();
    assert_eq!(daemon.send_attempts.lock().len(), 1);
    assert_eq!(inbox_records(dir.path()).last().unwrap()["state"], "done");
}

#[tokio::test]
async fn poison_update_is_dead_lettered_once_and_later_updates_flow() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([
            text_update(61, PAIRED_USER_ID, TEST_CHAT_ID, "poison"),
            text_update(62, PAIRED_USER_ID, TEST_CHAT_ID, "healthy"),
        ]),
    )
    .await;
    let daemon = one_bot_daemon();
    daemon.send_failures.lock().insert(
        "poison".into(),
        VecDeque::from([crate::TelegramError::DaemonHttp {
            status: 400,
            message: "invalid message".into(),
        }]),
    );
    let dir = tempfile::tempdir().unwrap();
    let cfg = durable_config(&server, dir.path());
    let channel = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    assert!(channel.poll_once().await.is_err());
    assert_eq!(*daemon.send_attempts.lock(), vec!["poison", "healthy"]);
    drop(channel);
    TelegramChannel::new(cfg, daemon.clone())
        .unwrap()
        .poll_once()
        .await
        .unwrap();
    assert_eq!(daemon.sent_messages.lock().len(), 1);
    let records = inbox_records(dir.path());
    let poison = records.iter().rev().find(|r| r["update_id"] == 61).unwrap();
    assert_eq!(poison["state"], "dead");
    assert_eq!(poison["attempts"], 1);
    let texts = sent_texts(&server.received_requests().await.unwrap());
    assert_eq!(
        texts
            .iter()
            .filter(|s| s.contains("Could not process message 61."))
            .count(),
        1
    );
}

#[tokio::test]
async fn retry_limit_is_durable_and_notifies_once() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([text_update(71, PAIRED_USER_ID, TEST_CHAT_ID, "offline")]),
    )
    .await;
    let daemon = one_bot_daemon();
    daemon.send_failures.lock().insert(
        "offline".into(),
        (0..8)
            .map(|_| crate::TelegramError::Daemon("offline".into()))
            .collect(),
    );
    let dir = tempfile::tempdir().unwrap();
    let channel =
        TelegramChannel::new(durable_config(&server, dir.path()), daemon.clone()).unwrap();
    for _ in 0..8 {
        assert!(channel.poll_once().await.is_err());
        channel.advance_retry_clock(Duration::from_secs(300));
    }
    channel.poll_once().await.unwrap();
    assert_eq!(daemon.send_attempts.lock().len(), 8);
    let records = inbox_records(dir.path());
    let last = records.last().unwrap();
    assert_eq!(last["state"], "dead");
    assert_eq!(last["attempts"], 8);
    assert_eq!(
        sent_texts(&server.received_requests().await.unwrap())
            .iter()
            .filter(|s| s.contains("Could not process message 71."))
            .count(),
        1
    );
}

#[tokio::test]
async fn offset_cannot_advance_past_a_failed_inbox_write() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([text_update(
            81,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "durable first"
        )]),
    )
    .await;
    let daemon = one_bot_daemon();
    let dir = tempfile::tempdir().unwrap();
    let channel =
        TelegramChannel::new(durable_config(&server, dir.path()), daemon.clone()).unwrap();
    channel.fail_inbox_writes(true);
    assert!(channel.poll_once().await.is_err());
    assert_eq!(channel.pairing().saved_offset("123456789"), None);
    assert!(daemon.send_attempts.lock().is_empty());
    assert_eq!(inbox_records(dir.path()), Vec::<Value>::new());
    channel.fail_inbox_writes(false);
    channel.poll_once().await.unwrap();
    assert_eq!(channel.pairing().saved_offset("123456789"), Some(82));
    let requests = server.received_requests().await.unwrap();
    let offsets: Vec<Value> = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/getUpdates"))
        .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["offset"].clone())
        .collect();
    assert_eq!(offsets, vec![Value::Null, Value::Null]);
}

#[tokio::test]
async fn failed_offset_save_keeps_durable_inbox_and_published_offset_safe() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([text_update(91, PAIRED_USER_ID, TEST_CHAT_ID, "retry state")]),
    )
    .await;
    let daemon = one_bot_daemon();
    let dir = tempfile::tempdir().unwrap();
    let channel =
        TelegramChannel::new(durable_config(&server, dir.path()), daemon.clone()).unwrap();
    channel.pairing().fail_saves(true);
    assert!(channel.poll_once().await.is_err());
    assert_eq!(channel.pairing().saved_offset("123456789"), None);
    assert!(
        inbox_records(dir.path())
            .iter()
            .any(|r| r["update_id"] == 91 && r["state"] == "pending")
    );
    channel.pairing().fail_saves(false);
    channel.advance_retry_clock(Duration::from_secs(2));
    channel.poll_once().await.unwrap();
    assert_eq!(channel.pairing().saved_offset("123456789"), Some(92));
    assert_eq!(daemon.sent_messages.lock().len(), 1);
}

// Audit REL-15: Stop's daemon cancellation must happen while a previous Telegram
// send is still blocked. The deadline uses paused Tokio time after the send starts.
#[tokio::test]
async fn stop_cancels_run_while_telegram_send_is_blocked() {
    let server = MockServer::start().await;
    let send_started = Arc::new(tokio::sync::Notify::new());
    let observed = send_started.clone();
    Mock::given(method("POST")).and(path(tg_endpoint("sendMessage")))
        .respond_with(move |request: &Request| {
            observed.notify_one();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": {"message_id": 1, "chat": {"id": body["chat_id"]}, "text": body["text"]}})).set_delay(Duration::from_secs(3600))
        }).mount(&server).await;
    let daemon = one_bot_daemon();
    let run = RunId::new();
    let tx = daemon.queue_open_run(run);
    tx.send(Envelope::new(
        0,
        Event::MessageDelta(MessageDelta {
            run_id: run,
            text: "blocked output ".into(),
        }),
    ))
    .unwrap();
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(server.uri())
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();
    let first = channel.clone();
    let first = tokio::spawn(async move {
        first
            .handle_update(update(text_update(
                101,
                PAIRED_USER_ID,
                TEST_CHAT_ID,
                "start",
            )))
            .await
    });
    send_started.notified().await;
    tokio::time::pause();
    let stop = channel.clone();
    let stop = tokio::spawn(async move {
        stop.handle_update(update(text_update(
            102,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/stop",
        )))
        .await
    });
    let cancelled =
        tokio::time::timeout(Duration::from_secs(1), daemon.cancel_seen.notified()).await;
    channel.stop();
    first.await.unwrap().unwrap();
    stop.await.unwrap().unwrap();
    assert!(
        cancelled.is_ok(),
        "Stop waited behind the blocked Telegram send"
    );
    assert_eq!(*daemon.cancelled_runs.lock(), vec![run]);
    tokio::time::resume();
}

#[tokio::test]
async fn ready_event_stream_yields_at_the_drain_quota() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let run = RunId::new();
    let polled = Arc::new(AtomicUsize::new(0));
    let count = polled.clone();
    let events = (0..100).map(move |seq| {
        Envelope::new(
            seq,
            Event::MessageDelta(MessageDelta {
                run_id: run,
                text: "x".into(),
            }),
        )
    });
    let stream = stream::iter(events)
        .inspect(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .chain(stream::pending());
    daemon.queue_run_stream(run, Box::pin(stream));
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(server.uri())
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();
    tokio::time::pause();
    channel
        .handle_update(update(text_update(
            111,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "start",
        )))
        .await
        .unwrap();
    assert!(
        (1..=32).contains(&polled.load(Ordering::SeqCst)),
        "one drain consumed the whole ready stream"
    );
    tokio::time::resume();
    channel
        .handle_update(update(text_update(
            112,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/stop",
        )))
        .await
        .unwrap();
    assert_eq!(*daemon.cancelled_runs.lock(), vec![run]);
}

#[tokio::test]
async fn daemon_delivery_sends_stable_idempotency_header_and_preserves_status() {
    use wiremock::matchers::header;
    let server = MockServer::start().await;
    let chat = ChatId::new();
    Mock::given(method("POST"))
        .and(path(format!("/v1/chats/{}/messages", chat.0)))
        .and(header("idempotency-key", "tg:121"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"error": {"message": "conflicting payload"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpDaemonClient::new(server.uri(), SecretValue::new("fake-token"));
    assert!(matches!(
        client
            .send_message_idempotent(chat, "hello", "tg:121")
            .await,
        Err(crate::TelegramError::DaemonHttp { status: 409, .. })
    ));
}

// Audit REL-16: an oversized chunked body never finishes, so a post-read size
// check would hang. Deliver bytes first, then use a paused clock for the bound.
#[tokio::test]
async fn oversized_chunked_telegram_body_is_rejected_before_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
    let (end_tx, end_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        let body = vec![b'x'; 64 * 1024];
        for _ in 0..129 {
            if socket.write_all(b"10000\r\n").await.is_err()
                || socket.write_all(&body).await.is_err()
                || socket.write_all(b"\r\n").await.is_err()
            {
                break;
            }
        }
        let _ = sent_tx.send(());
        // Intentionally no terminal chunk. The client must stop reading at the cap.
        let _ = end_rx.await;
    });
    let client = crate::BotApiClient::with_base_url(
        SecretValue::new(TEST_BOT_TOKEN),
        format!("http://{address}"),
    );
    let request = tokio::spawn(async move { client.get_updates(None, 0).await });
    sent_rx.await.unwrap();
    tokio::time::pause();
    let result = tokio::time::timeout(Duration::from_secs(1), request).await;
    tokio::time::resume();
    let _ = end_tx.send(());
    server.await.unwrap();
    let error = result
        .expect("reader waited for EOF after exceeding cap")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("response too large"), "{error}");
}

// A regression using only pre-fix public APIs, so the same test can run unchanged
// on main: after Telegram confirms the offset, a transient daemon error must not
// lose the message. Only the retry clock advances; all HTTP stays on loopback.
#[tokio::test]
async fn failed_daemon_delivery_is_retried_after_poll_offset_advances() {
    let telegram = MockServer::start().await;
    mount_telegram_mocks(&telegram).await;
    Mock::given(method("POST")).and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": [text_update(131, PAIRED_USER_ID, TEST_CHAT_ID, "must survive")]})))
        .up_to_n_times(1).mount(&telegram).await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("getUpdates")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": []})))
        .mount(&telegram)
        .await;
    let daemon = MockServer::start().await;
    let bot = BotId::new();
    let chat = ChatId::new();
    Mock::given(method("POST"))
        .and(path("/v1/chats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": chat.0, "bot_id": bot.0, "title": "telegram"})),
        )
        .mount(&daemon)
        .await;
    let message_path = format!("/v1/chats/{}/messages", chat.0);
    Mock::given(method("POST"))
        .and(path(&message_path))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error": {"message": "temporary failure"}})),
        )
        .up_to_n_times(1)
        .mount(&daemon)
        .await;
    Mock::given(method("POST"))
        .and(path(&message_path))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"delivery": "queued", "run_id": null})),
        )
        .mount(&daemon)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&daemon)
        .await;
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(telegram.uri())
            .with_paired_users([PAIRED_USER_ID])
            .with_default_bot(bot),
        Arc::new(HttpDaemonClient::new(
            daemon.uri(),
            SecretValue::new("fake-daemon-token"),
        )),
    )
    .unwrap();
    assert!(channel.poll_once().await.is_err());
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    channel.poll_once().await.unwrap();
    let requests = daemon.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == message_path)
            .count(),
        2,
        "the failed update was acknowledged to Telegram and then lost"
    );
}

#[tokio::test]
async fn stop_retries_its_persisted_run_target_after_daemon_failure() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    let daemon = one_bot_daemon();
    let run = RunId::new();
    let _tx = daemon.queue_open_run(run);
    let dir = tempfile::tempdir().unwrap();
    let cfg = durable_config(&server, dir.path());
    let first = TelegramChannel::new(cfg.clone(), daemon.clone()).unwrap();
    first
        .handle_update(update(text_update(
            141,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "start",
        )))
        .await
        .unwrap();
    daemon
        .cancel_failures
        .lock()
        .push_back(crate::TelegramError::Daemon("offline".into()));
    assert!(
        first
            .handle_update(update(text_update(
                142,
                PAIRED_USER_ID,
                TEST_CHAT_ID,
                "/stop"
            )))
            .await
            .is_err()
    );
    assert!(daemon.cancelled_runs.lock().is_empty());
    drop(first);
    let restarted = TelegramChannel::new(cfg, daemon.clone()).unwrap();
    restarted.advance_retry_clock(Duration::from_secs(2));
    restarted
        .handle_update(update(text_update(
            142,
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            "/stop",
        )))
        .await
        .unwrap();
    assert_eq!(*daemon.cancelled_runs.lock(), vec![run]);
}

#[tokio::test]
async fn priority_stop_does_not_start_older_messages_from_the_same_batch() {
    let server = MockServer::start().await;
    mount_telegram_mocks(&server).await;
    mock_updates(
        &server,
        json!([
            text_update(151, PAIRED_USER_ID, TEST_CHAT_ID, "start something"),
            text_update(152, PAIRED_USER_ID, TEST_CHAT_ID, "/stop"),
            text_update(153, PAIRED_USER_ID, TEST_CHAT_ID, "later new request"),
        ]),
    )
    .await;
    let daemon = one_bot_daemon();
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
            .with_api_base_url(server.uri())
            .with_paired_users([PAIRED_USER_ID]),
        daemon.clone(),
    )
    .unwrap();
    channel.poll_once().await.unwrap();
    assert_eq!(
        daemon
            .sent_messages
            .lock()
            .iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>(),
        vec!["later new request"]
    );
}

async fn approval_messages(details: Option<serde_json::Value>) -> Vec<serde_json::Value> {
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
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::ApprovalNeeded(ApprovalNeeded {
            run_id,
            call_id: ToolCallId::new(),
            tool_name: "schedule_create".into(),
            input: json!({}),
            summary: "schedule_create {\"prompt\":\"cut at 300\"".into(),
            details,
        }),
    ))
    .unwrap();
    daemon.queue_run_stream(
        run_id,
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })),
    );
    channel
        .handle_update(
            serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")).unwrap(),
        )
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    sent_messages(&reqs)
}

fn schedule_card(prompt: &str) -> serde_json::Value {
    json!({
        "kind": "schedule", "action": "create", "name": "Standup",
        "prompt": prompt, "schedule": "Every weekday at 10:30", "timezone": "Europe/Istanbul",
        "next_runs": ["2026-11-13T10:30:00+03:00", "2026-11-16T10:30:00+03:00", "2026-11-17T10:30:00+03:00"],
        "permission": {"level": "ask_first", "allow": [], "ask": ["fs"], "deny": []}
    })
}

#[tokio::test]
async fn schedule_approval_shows_everything_without_always_and_chunks_long_prompts() {
    let prompt = format!("{}\nTAIL-MARKER", "long prompt line. ".repeat(400));
    let msgs = approval_messages(Some(schedule_card(&prompt))).await;
    assert!(msgs.len() >= 2, "a long prompt spans several messages");
    let all: String = msgs.iter().map(|m| m["text"].as_str().unwrap()).collect();
    for want in [
        "Every weekday at 10:30 (Europe/Istanbul)",
        "2026-11-17T10:30:00+03:00",
        "permissions: ask_first",
        "TAIL-MARKER",
    ] {
        assert!(all.contains(want), "{want} missing");
    }
    for m in &msgs[..msgs.len() - 1] {
        assert!(
            m.get("reply_markup").is_none(),
            "buttons only on the last message"
        );
    }
    let kb = &msgs[msgs.len() - 1]["reply_markup"]["inline_keyboard"][0];
    let labels: Vec<&str> = kb
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Approve once", "Deny"], "no Always for a schedule");
}

#[tokio::test]
async fn unrenderable_details_leave_approval_to_the_desktop_app() {
    let msgs = approval_messages(Some(json!({"kind": "future-thing"}))).await;
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0]["text"].as_str().unwrap().contains("desktop app"));
    assert!(msgs[0].get("reply_markup").is_none(), "no approve button");
}

#[tokio::test]
async fn plain_approvals_keep_their_three_buttons() {
    let msgs = approval_messages(None).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0]["reply_markup"]["inline_keyboard"][0]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

fn once_only_cb_args() -> (RunId, ToolCallId) {
    (RunId::new(), ToolCallId::new())
}

#[tokio::test]
async fn a_crafted_always_callback_for_a_schedule_approval_never_reaches_the_daemon() {
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
    let (run_id, call_id) = once_only_cb_args();
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::ApprovalNeeded(ApprovalNeeded {
            run_id,
            call_id,
            tool_name: "schedule_create".into(),
            input: json!({}),
            summary: "s".into(),
            details: Some(schedule_card("short prompt")),
        }),
    ))
    .unwrap();
    daemon.queue_run_stream(
        run_id,
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })),
    );
    channel
        .handle_update(
            serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")).unwrap(),
        )
        .await
        .unwrap();
    // Crafted: the user (or an attacker with the callback format) sends "always" / "this chat".
    for (i, action) in ["always", "this_chat", "always_category"]
        .into_iter()
        .enumerate()
    {
        let data = format_approval_callback(call_id, action);
        let u: Update = serde_json::from_value(callback_update(
            10 + i64::try_from(i).unwrap(),
            PAIRED_USER_ID,
            TEST_CHAT_ID,
            &format!("cb-{i}"),
            &data,
        ))
        .unwrap();
        channel.handle_update(u).await.unwrap();
    }
    assert!(daemon.approvals.lock().is_empty(), "no decision was sent");
    let reqs = server.received_requests().await.unwrap();
    assert!(
        callback_answers(&reqs)
            .iter()
            .any(|a| a.to_string().contains("only be approved once"))
    );
    // The real button still works, once.
    let ok: Update = serde_json::from_value(callback_update(
        20,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "cb-ok",
        &format_approval_callback(call_id, "once"),
    ))
    .unwrap();
    channel.handle_update(ok).await.unwrap();
    assert_eq!(daemon.approvals.lock().len(), 1);
}

#[tokio::test]
async fn a_failing_middle_chunk_leaves_no_button_and_points_to_the_desktop() {
    let server = MockServer::start().await;
    // The first message goes through, every later one fails.
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    Mock::given(method("POST"))
        .and(path(tg_endpoint("sendMessage")))
        .respond_with(move |req: &Request| {
            let n = c.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if n == 0 || body["text"].as_str().is_some_and(|t| t.contains("could not be shown")) {
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": {"message_id": 100 + n, "chat": {"id": TEST_CHAT_ID, "type": "private"}, "text": ""}
                }))
            } else {
                ResponseTemplate::new(500).set_body_json(json!({"ok": false, "description": "boom"}))
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(tg_endpoint("answerCallbackQuery")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": true})))
        .mount(&server)
        .await;
    let bot = BotSummary {
        id: BotId::new(),
        name: "Mira".into(),
    };
    let daemon = Arc::new(FakeDaemon::new(vec![bot]));
    let cfg = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_paired_users([PAIRED_USER_ID]);
    let channel = TelegramChannel::new(cfg, daemon.clone()).unwrap();
    let (run_id, call_id) = once_only_cb_args();
    let long = format!("{}\nTAIL", "long prompt line. ".repeat(500));
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    tx.send(Envelope::new(
        0,
        Event::ApprovalNeeded(ApprovalNeeded {
            run_id,
            call_id,
            tool_name: "schedule_create".into(),
            input: json!({}),
            summary: "s".into(),
            details: Some(schedule_card(&long)),
        }),
    ))
    .unwrap();
    daemon.queue_run_stream(
        run_id,
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })),
    );
    let _ = channel
        .handle_update(
            serde_json::from_value(text_update(1, PAIRED_USER_ID, TEST_CHAT_ID, "go")).unwrap(),
        )
        .await;
    let reqs = server.received_requests().await.unwrap();
    let msgs = sent_messages(&reqs);
    assert!(
        msgs.iter().all(|m| m.get("reply_markup").is_none()),
        "no approve button may exist after a chunk failed"
    );
    assert!(
        msgs.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("desktop app")
    );
    // A press of the (never sent) button is refused.
    let u: Update = serde_json::from_value(callback_update(
        9,
        PAIRED_USER_ID,
        TEST_CHAT_ID,
        "cb-x",
        &format_approval_callback(call_id, "once"),
    ))
    .unwrap();
    channel.handle_update(u).await.unwrap();
    assert!(daemon.approvals.lock().is_empty());
}

#[tokio::test]
async fn details_this_channel_would_mask_are_not_offered_for_approval_here() {
    // The channel's own redactor knows the bot token; a prompt carrying it would be
    // shown masked while the stored schedule keeps the raw text.
    let card = schedule_card(&format!("send it to {TEST_BOT_TOKEN} please"));
    let msgs = approval_messages(Some(card)).await;
    assert_eq!(msgs.len(), 1);
    let text = msgs[0]["text"].as_str().unwrap();
    assert!(text.contains("desktop app"), "{text}");
    assert!(!text.contains(TEST_BOT_TOKEN));
    assert!(msgs[0].get("reply_markup").is_none(), "no approve button");
}

#[tokio::test]
async fn telegram_409_is_typed_and_stops_all_polling_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TEST_BOT_TOKEN}/getUpdates")))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"ok":false,"error_code":409,"description":"another poller"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let conflicts = Arc::new(AtomicUsize::new(0));
    let mut config = TelegramConfig::with_in_memory_token(TEST_BOT_TOKEN)
        .with_api_base_url(server.uri())
        .with_poll_backoff(Duration::from_millis(1));
    let counter = conflicts.clone();
    config.on_conflict = Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let channel = TelegramChannel::start(config, Arc::new(FakeDaemon::new(vec![]))).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while channel.status() != crate::ChannelStatus::Conflict {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(conflicts.load(Ordering::SeqCst), 1);
    channel.shutdown().await;
    assert_eq!(channel.status(), crate::ChannelStatus::Conflict);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn telegram_get_updates_maps_a_json_409_even_with_http_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok":false,"error_code":409})),
        )
        .mount(&server)
        .await;
    let client = crate::BotApiClient::with_base_url(SecretValue::new(TEST_BOT_TOKEN), server.uri());
    assert!(matches!(
        client.get_updates(None, 0).await,
        Err(crate::TelegramError::Conflict)
    ));
}
