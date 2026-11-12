//! Small trait (`DaemonClient` / `ApiClient`) over the daemon HTTP API so
//! [`crate::TelegramChannel`] can run in-process or out of process, plus the
//! HTTP/SSE [`HttpDaemonClient`] implementation.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures_util::StreamExt;
use protocol::{BotId, ChatId, Envelope, Event, RunId, ToolCallId};
use reqwest::{Client, Method};
use secret_store::{REDACTED, SecretValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{Result, TelegramError};

/// Summary of a bot returned by `GET /v1/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSummary {
    /// Bot identifier.
    pub id: BotId,
    /// Human-readable display name.
    pub name: String,
}

/// Summary of a chat thread returned by `POST /v1/chats`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSummary {
    /// Chat identifier.
    pub id: ChatId,
    /// Bot this chat talks to.
    pub bot_id: BotId,
    /// Chat title.
    pub title: String,
}

/// How `POST /v1/chats/{id}/messages` delivered a user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// A new turn (run) started.
    NewTurn,
    /// Injected into the running turn.
    Steered,
    /// Queued behind the running turn.
    Queued,
    /// The message was `/stop` and cancelled the running turn.
    Stopped,
}

/// Response of `POST /v1/chats/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendOutcome {
    /// Delivery disposition.
    pub delivery: DeliveryKind,
    /// Run created or affected (`None` when queued).
    pub run_id: Option<RunId>,
}

/// Approval decision (`POST /v1/approvals/{call_id}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Allow the tool call.
    Approve,
    /// Deny the tool call.
    Deny,
}

/// Scope of an approval (`POST /v1/approvals/{call_id}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// Approve this single tool call only.
    #[default]
    Once,
    /// Approve this tool for the remainder of the chat.
    ThisChat,
    /// Persist a grant for this tool on the bot.
    Always,
    /// Persist a grant for the tool's category on the bot.
    AlwaysCategory,
}

/// Daemon status summary for `/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Health status string (typically `"ok"`).
    pub status: String,
    /// Daemon version string.
    pub version: String,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Number of approvals currently waiting for a decision.
    pub pending_approvals: usize,
}

/// Trait abstracting the daemon `/v1` HTTP + SSE API so [`crate::TelegramChannel`]
/// can run either in-process (backed by a direct adapter or test mock) or
/// out-of-process (backed by [`HttpDaemonClient`]).
#[async_trait]
pub trait DaemonClient: Send + Sync {
    /// `GET /v1/bots`: list all configured bots.
    async fn list_bots(&self) -> Result<Vec<BotSummary>>;

    /// `POST /v1/chats`: create a chat thread for `bot_id`.
    async fn create_chat(&self, bot_id: BotId, title: Option<&str>) -> Result<ChatSummary>;

    /// `POST /v1/chats/{chat_id}/messages`: send a user message to `chat_id`.
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<SendOutcome>;

    /// Deliver with a stable key such as `tg:123`. HTTP adapters forward this in
    /// `Idempotency-Key`; in-process adapters can override it when task 136 is available.
    ///
    /// The compatibility default cannot deduplicate an ambiguous remote acceptance.
    async fn send_message_idempotent(
        &self,
        chat_id: ChatId,
        text: &str,
        _idempotency_key: &str,
    ) -> Result<SendOutcome> {
        self.send_message(chat_id, text).await
    }

    /// `POST /v1/approvals/{call_id}`: approve or deny a pending tool call.
    async fn decide_approval(
        &self,
        call_id: ToolCallId,
        decision: ApprovalDecision,
        scope: ApprovalScope,
        reason: Option<&str>,
    ) -> Result<()>;

    /// `POST /v1/questions/{call_id}`: answer a pending `ask_user` question.
    async fn answer_question(&self, call_id: ToolCallId, answer: &str) -> Result<()>;

    /// `POST /v1/runs/{run_id}/cancel`: cancel an active run.
    async fn cancel_run(&self, run_id: RunId) -> Result<()>;

    /// `GET /v1/health` + `GET /v1/approvals`: query daemon health and pending approvals.
    async fn status(&self) -> Result<DaemonStatus>;

    /// `GET /v1/events?run_id={run_id}`: subscribe to the event stream of `run_id`.
    async fn subscribe_run(&self, run_id: RunId) -> Result<BoxStream<'static, Envelope>>;

    /// Find the oldest top-level (non-subagent) run of daemon chat `chat_id` that was
    /// created at or after `created_after_ms` (Unix ms) and is not in `exclude`, in any
    /// state (`queued`, `running`, ... or already ended: `subscribe_run` replays the log,
    /// so a queued turn that finished before it was found is still shown).
    ///
    /// Used to attach to a turn that was queued behind another run: the daemon starts it
    /// later as a new run nobody subscribed to. The default implementation reports nothing.
    async fn find_chat_run(
        &self,
        _chat_id: ChatId,
        _exclude: &[RunId],
        _created_after_ms: i64,
    ) -> Result<Option<RunId>> {
        Ok(None)
    }
}

/// Alias for [`DaemonClient`] matching `TelegramChannel::start(config, api_client)`.
pub type ApiClient = dyn DaemonClient;

/// Out-of-process [`DaemonClient`] implementation over the daemon's `/v1` HTTP and SSE routes.
#[derive(Clone)]
pub struct HttpDaemonClient {
    base_url: String,
    token: SecretValue,
    http: Client,
}

impl fmt::Debug for HttpDaemonClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpDaemonClient")
            .field("base_url", &self.base_url)
            .field("token", &REDACTED)
            .finish_non_exhaustive()
    }
}

impl HttpDaemonClient {
    /// Create a client targeting `base_url` (e.g. `"http://127.0.0.1:47821"`) with `token`.
    #[must_use]
    pub fn new(base_url: impl Into<String>, token: SecretValue) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Self {
            base_url,
            token,
            http: Client::new(),
        }
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        self.request_with_key(method, path, body, None).await
    }

    async fn request_with_key(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(self.token.expose())
            .timeout(Duration::from_secs(30));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        if let Some(key) = idempotency_key {
            rb = rb.header("Idempotency-Key", key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| TelegramError::Daemon(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TelegramError::Daemon(e.to_string()))?;
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = val["error"]["message"]
                .as_str()
                .map_or_else(|| status.to_string(), str::to_owned);
            return Err(TelegramError::DaemonHttp {
                status: status.as_u16(),
                message: msg,
            });
        }
        Ok(val)
    }
}

/// Upper bound for one unterminated SSE block (protects against a misbehaving peer).
const MAX_SSE_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// Byte-level SSE block splitter.
///
/// Network chunks may cut a multi-byte UTF-8 character (or a CR LF pair) in half, so
/// the buffer holds raw bytes and a block is decoded only once it is complete.
#[derive(Debug, Default)]
pub(crate) struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    /// Append a raw network chunk.
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Whether more than [`MAX_SSE_BLOCK_BYTES`] are buffered without a block boundary.
    pub(crate) fn overflowed(&self) -> bool {
        self.buf.len() > MAX_SSE_BLOCK_BYTES
    }

    /// Remove and decode the next complete block (terminated by a blank line), if any.
    pub(crate) fn next_block(&mut self) -> Option<String> {
        let end = Self::block_end(&self.buf)?;
        let raw: Vec<u8> = self.buf.drain(..end).collect();
        Some(String::from_utf8_lossy(&raw).into_owned())
    }

    /// Index just past the first blank line (LF LF, CR LF CR LF, or a mix).
    fn block_end(buf: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < buf.len() {
            if buf[i] == b'\n' {
                match buf.get(i + 1) {
                    Some(b'\n') => return Some(i + 2),
                    Some(b'\r') if buf.get(i + 2) == Some(&b'\n') => return Some(i + 3),
                    _ => {}
                }
            }
            i += 1;
        }
        None
    }
}

/// Extract the joined `data:` payload of one SSE block.
fn sse_data(block: &str) -> Option<String> {
    let data: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
        .collect();
    (!data.is_empty()).then(|| data.join("\n"))
}

#[async_trait]
impl DaemonClient for HttpDaemonClient {
    async fn list_bots(&self) -> Result<Vec<BotSummary>> {
        let v = self.request(Method::GET, "/v1/bots", None).await?;
        serde_json::from_value(v).map_err(|e| TelegramError::Malformed(e.to_string()))
    }

    async fn create_chat(&self, bot_id: BotId, title: Option<&str>) -> Result<ChatSummary> {
        let body = match title {
            Some(t) => json!({ "bot_id": bot_id.0, "title": t }),
            None => json!({ "bot_id": bot_id.0 }),
        };
        let v = self.request(Method::POST, "/v1/chats", Some(body)).await?;
        serde_json::from_value(v).map_err(|e| TelegramError::Malformed(e.to_string()))
    }

    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<SendOutcome> {
        let body = json!({ "text": text, "mode": "queue" });
        let v = self
            .request(
                Method::POST,
                &format!("/v1/chats/{}/messages", chat_id.0),
                Some(body),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| TelegramError::Malformed(e.to_string()))
    }

    async fn send_message_idempotent(
        &self,
        chat_id: ChatId,
        text: &str,
        idempotency_key: &str,
    ) -> Result<SendOutcome> {
        let body = json!({ "text": text, "mode": "queue" });
        let v = self
            .request_with_key(
                Method::POST,
                &format!("/v1/chats/{}/messages", chat_id.0),
                Some(body),
                Some(idempotency_key),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| TelegramError::Malformed(e.to_string()))
    }

    async fn decide_approval(
        &self,
        call_id: ToolCallId,
        decision: ApprovalDecision,
        scope: ApprovalScope,
        reason: Option<&str>,
    ) -> Result<()> {
        let body = match decision {
            ApprovalDecision::Approve => json!({
                "decision": "approve",
                "scope": scope,
            }),
            ApprovalDecision::Deny => json!({
                "decision": "deny",
                "reason": reason,
            }),
        };
        let _ = self
            .request(
                Method::POST,
                &format!("/v1/approvals/{}", call_id.0),
                Some(body),
            )
            .await?;
        Ok(())
    }

    async fn answer_question(&self, call_id: ToolCallId, answer: &str) -> Result<()> {
        let body = json!({ "answer": answer });
        let _ = self
            .request(
                Method::POST,
                &format!("/v1/questions/{}", call_id.0),
                Some(body),
            )
            .await?;
        Ok(())
    }

    async fn cancel_run(&self, run_id: RunId) -> Result<()> {
        let _ = self
            .request(Method::POST, &format!("/v1/runs/{}/cancel", run_id.0), None)
            .await?;
        Ok(())
    }

    async fn status(&self) -> Result<DaemonStatus> {
        let health = self.request(Method::GET, "/v1/health", None).await?;
        let approvals = self.request(Method::GET, "/v1/approvals", None).await?;
        let pending_approvals = approvals.as_array().map_or(0, Vec::len);
        Ok(DaemonStatus {
            status: health["status"].as_str().unwrap_or("unknown").to_owned(),
            version: health["version"].as_str().unwrap_or("unknown").to_owned(),
            uptime_secs: health["uptime_secs"].as_u64().unwrap_or(0),
            pending_approvals,
        })
    }

    async fn find_chat_run(
        &self,
        chat_id: ChatId,
        exclude: &[RunId],
        created_after_ms: i64,
    ) -> Result<Option<RunId>> {
        let v = self.request(Method::GET, "/v1/runs", None).await?;
        let runs = v.as_array().map_or(&[][..], Vec::as_slice);
        let mut best: Option<(i64, RunId)> = None;
        for r in runs {
            let Some(id) = r["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                continue;
            };
            let run_id = RunId(id);
            let same_chat =
                r["chat_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) == Some(chat_id.0);
            let created = r["created_at_ms"].as_i64().unwrap_or(0);
            if same_chat
                && r["parent_run_id"].is_null()
                && created >= created_after_ms
                && !exclude.contains(&run_id)
                && best.is_none_or(|(c, _)| created < c)
            {
                best = Some((created, run_id));
            }
        }
        Ok(best.map(|(_, id)| id))
    }

    async fn subscribe_run(&self, run_id: RunId) -> Result<BoxStream<'static, Envelope>> {
        let resp = self
            .http
            .get(format!("{}/v1/events?run_id={}", self.base_url, run_id.0))
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|e| TelegramError::Daemon(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TelegramError::Daemon(format!(
                "SSE stream failed with {}",
                resp.status()
            )));
        }

        let byte_stream = resp.bytes_stream();
        let stream = futures::stream::unfold(
            (byte_stream, SseBuffer::default(), false),
            |(mut bytes, mut buf, mut done)| async move {
                if done {
                    return None;
                }
                loop {
                    if let Some(block) = buf.next_block() {
                        if let Some(data) = sse_data(&block)
                            && let Ok(env) = serde_json::from_str::<Envelope>(&data)
                        {
                            if matches!(env.event, Event::RunFinished(_)) {
                                done = true;
                            }
                            return Some((env, (bytes, buf, done)));
                        }
                        continue;
                    }
                    if buf.overflowed() {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => buf.push(&chunk),
                        _ => return None,
                    }
                }
            },
        );
        // `fuse`: the unfold above must never be polled again after it ended.
        let stream = stream.fuse();
        Ok(Box::pin(stream))
    }
}

/// Parse an approval button's `callback_data` string (`"approve:<action>:<call_id>"`).
#[must_use]
pub fn parse_approval_callback(
    data: &str,
) -> Option<(ToolCallId, ApprovalDecision, ApprovalScope)> {
    let rest = data
        .strip_prefix("approve:")
        .or_else(|| data.strip_prefix("ap:"))?;
    let (action, id_str) = rest.split_once(':')?;
    let uuid = Uuid::parse_str(id_str.trim()).ok()?;
    let call_id = ToolCallId(uuid);
    match action {
        "once" => Some((call_id, ApprovalDecision::Approve, ApprovalScope::Once)),
        "always" => Some((call_id, ApprovalDecision::Approve, ApprovalScope::Always)),
        "deny" => Some((call_id, ApprovalDecision::Deny, ApprovalScope::Once)),
        _ => None,
    }
}

/// Format an approval button's `callback_data` string.
#[must_use]
pub fn format_approval_callback(call_id: ToolCallId, action: &str) -> String {
    format!("approve:{action}:{}", call_id.0)
}

/// Parse a question button's `callback_data` string (`"question:<call_id>:<index_or_answer>"`).
#[must_use]
pub fn parse_question_callback(data: &str) -> Option<(ToolCallId, String)> {
    let rest = data
        .strip_prefix("question:")
        .or_else(|| data.strip_prefix("q:"))?;
    let (id_str, payload) = rest.split_once(':')?;
    let uuid = Uuid::parse_str(id_str.trim()).ok()?;
    Some((ToolCallId(uuid), payload.to_owned()))
}

/// Format a question option button's `callback_data` string using the option index.
#[must_use]
pub fn format_question_callback(call_id: ToolCallId, option_index: usize) -> String {
    format!("question:{}:{option_index}", call_id.0)
}
