//! Replay a real loop approval through the production Telegram card/callback flow.
//! Only loopback mock HTTP is used; there is no Telegram connection or token.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use channel_telegram::{
    ApprovalDecision, ApprovalScope, BotSummary, ChatSummary, DaemonClient, DaemonStatus,
    DeliveryKind, Result, SendOutcome, TelegramChannel, TelegramConfig, format_approval_callback,
};
use futures::stream::{BoxStream, StreamExt};
use protocol::{BotId, ChatId, Envelope, Event, RunId, ToolCallId};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::harness::TurnTrace;

pub(crate) async fn check(
    trace: &TurnTrace,
    buttons: bool,
    failures: &mut Vec<String>,
) -> crate::Result<()> {
    let Some(card) = trace.events.iter().find_map(|e| match e {
        Event::ApprovalNeeded(a) => Some(a.clone()),
        _ => None,
    }) else {
        return Err("Telegram fixture needs an emitted approval".into());
    };
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|r: &wiremock::Request| {
            if r.url.path().ends_with("answerCallbackQuery") {
                return ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":true}));
            }
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":{
                "message_id":101,"chat":{"id":7001,"type":"private"},"text":body["text"]
            }}))
        })
        .mount(&server)
        .await;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(Envelope::new(0, Event::ApprovalNeeded(card.clone())))?;
    let daemon = Arc::new(OfflineDaemon {
        bot: BotId::new(),
        run: card.run_id,
        decisions: AtomicU64::new(0),
        stream: parking_lot::Mutex::new(Some(rx)),
    });
    let channel = TelegramChannel::new(
        TelegramConfig::with_in_memory_token("123456789:synthetic-eval-token")
            .with_api_base_url(server.uri())
            .with_paired_users([4242]),
        daemon.clone(),
    )?;
    channel
        .handle_update(serde_json::from_value(json!({"update_id":1,"message":{
            "message_id":10,"from":{"id":4242,"is_bot":false,"first_name":"Fixture"},
            "chat":{"id":7001,"type":"private"},"text":"review"
        }}))?)
        .await?;
    let requests = server.received_requests().await.unwrap_or_default();
    let offered = requests
        .iter()
        .filter(|r| r.url.path().ends_with("sendMessage"))
        .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .any(|v| v.get("reply_markup").is_some_and(|m| !m.is_null()));
    if offered != buttons {
        failures.push(format!(
            "Telegram buttons: got {offered}, expected {buttons}"
        ));
    }
    // Use distinct update IDs: this must exercise the single-use card gate,
    // rather than merely the incoming-update deduplicator.
    for id in 10..12 {
        channel.handle_update(serde_json::from_value(json!({"update_id":id,"callback_query":{
            "id":format!("callback-{id}"),"from":{"id":4242,"is_bot":false,"first_name":"Fixture"},
            "message":{"message_id":101,"chat":{"id":7001,"type":"private"},"text":"review"},
            "data":format_approval_callback(card.call_id,"once")
        }}))?).await?;
    }
    let decisions = daemon.decisions.load(Ordering::SeqCst);
    if decisions != u64::from(buttons) {
        failures.push(format!(
            "Telegram decisions after replay: {decisions}, expected {}",
            u64::from(buttons)
        ));
    }
    channel.shutdown().await;
    drop(tx);
    Ok(())
}

struct OfflineDaemon {
    bot: BotId,
    run: RunId,
    decisions: AtomicU64,
    stream: parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Envelope>>>,
}

#[async_trait]
impl DaemonClient for OfflineDaemon {
    async fn list_bots(&self) -> Result<Vec<BotSummary>> {
        Ok(vec![BotSummary {
            id: self.bot,
            name: "Fixture".into(),
        }])
    }
    async fn create_chat(&self, bot_id: BotId, _: Option<&str>) -> Result<ChatSummary> {
        Ok(ChatSummary {
            id: ChatId::new(),
            bot_id,
            title: "Fixture".into(),
        })
    }
    async fn send_message(&self, _: ChatId, _: &str) -> Result<SendOutcome> {
        Ok(SendOutcome {
            delivery: DeliveryKind::NewTurn,
            run_id: Some(self.run),
        })
    }
    async fn decide_approval(
        &self,
        _: ToolCallId,
        _: ApprovalDecision,
        _: ApprovalScope,
        _: Option<&str>,
    ) -> Result<()> {
        self.decisions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn answer_question(&self, _: ToolCallId, _: &str) -> Result<()> {
        Ok(())
    }
    async fn cancel_run(&self, _: RunId) -> Result<()> {
        Ok(())
    }
    async fn status(&self) -> Result<DaemonStatus> {
        Ok(DaemonStatus {
            status: "ok".into(),
            version: "fixture".into(),
            uptime_secs: 0,
            pending_approvals: 1,
        })
    }
    async fn subscribe_run(&self, _: RunId) -> Result<BoxStream<'static, Envelope>> {
        let rx = self.stream.lock().take().ok_or_else(|| {
            channel_telegram::TelegramError::Daemon("fixture stream already taken".into())
        })?;
        Ok(
            futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (v, rx)) })
                .boxed(),
        )
    }
}
