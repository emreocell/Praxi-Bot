//! Telegram ↔ Daemon bridge (`TelegramConfig`, `TelegramChannel`).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, Envelope, Event, RunId, ToolCallId};
use secret_store::{MemoryStore, Redactor, SecretRef, SecretStore};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::bot_api::{
    BotApiClient, DEFAULT_EDIT_INTERVAL, DEFAULT_TELEGRAM_API_URL, InlineKeyboardMarkup,
    ThrottledEditor, Update,
};
use crate::channel_trait::{
    Channel, ChannelCaps, ChatRef, IncomingKind, IncomingMessage, InlineButton, OutgoingMessage,
};
use crate::daemon_client::{
    ApprovalDecision, DaemonClient, DeliveryKind, format_approval_callback,
    format_question_callback, parse_approval_callback, parse_question_callback,
};
use crate::error::{Result, TelegramError};
use crate::inbox::{DeliveryReceipt, Inbox, MAX_ATTEMPTS};
use crate::pairing::{
    PairAttempt, PairingStore, TELEGRAM_BOT_TOKEN_SECRET, UNPAIRED_REFUSAL_TEXT, load_bot_token,
};

/// Default `getUpdates` long-polling timeout in seconds.
pub const DEFAULT_POLL_TIMEOUT_SECS: u64 = 25;

/// Default base interval between local drains of the active run streams (and base of the
/// `getUpdates` error backoff). It never shortens the `getUpdates` long poll itself.
pub const DEFAULT_POLL_BACKOFF: Duration = Duration::from_millis(100);

/// Upper bound of the drain interval while active runs produce no events (e.g. waiting
/// for an approval for hours).
const MAX_IDLE_DRAIN_INTERVAL: Duration = Duration::from_millis(500);

/// Maximum number of simultaneously tracked run streams (parents plus subagent children).
const MAX_ACTIVE_RUNS: usize = 64;

/// Bound of the remembered "already attached" run ids.
const MAX_ATTACHED_RUNS: usize = 4096;

/// How long a message queued behind another run is searched for its own run.
const QUEUED_TTL: Duration = Duration::from_secs(30 * 60);

/// Minimum interval between daemon lookups for a queued message's run.
const QUEUED_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Longest incoming text forwarded to the daemon (Telegram itself caps at 4096).
const MAX_INCOMING_CHARS: usize = 8192;

/// Maximum number of outstanding approval/question tokens remembered.
const MAX_PENDING_PROMPTS: usize = 1024;

/// Backoff after `409 Conflict` (another poller or a webhook is active).
const CONFLICT_BACKOFF: Duration = Duration::from_secs(15);

/// Upper bound of the exponential error backoff.
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(30);

/// A continuously ready SSE stream must yield back to inbound polling.
const MAX_DRAIN_EVENTS: usize = 32;
const MAX_EVENTS_PER_RUN: usize = 8;
const MAX_DRAIN_TIME: Duration = Duration::from_millis(25);
const MAX_INBOX_PER_PASS: usize = 32;

/// Delay before the next poll after `err` (the `failures`-th consecutive failure).
pub(crate) fn poll_error_delay(err: &TelegramError, failures: u32, base: Duration) -> Duration {
    match err {
        TelegramError::RateLimited { retry_after_secs } => {
            Duration::from_secs((*retry_after_secs).clamp(1, 120))
        }
        TelegramError::TelegramApi {
            error_code: Some(409),
            ..
        } => CONFLICT_BACKOFF,
        _ => {
            let shift = failures.saturating_sub(1).min(16);
            base.saturating_mul(1u32 << shift).min(MAX_ERROR_BACKOFF)
        }
    }
}

/// Configuration for [`TelegramChannel`].
#[derive(Clone)]
pub struct TelegramConfig {
    /// Secret store holding the Telegram bot token and other secrets to redact.
    pub secret_store: Arc<dyn SecretStore>,
    /// Secret reference pointing to the bot token inside `secret_store`.
    pub bot_token_secret: SecretRef,
    /// Base URL for the Telegram Bot API (defaults to `https://api.telegram.org`).
    pub api_base_url: String,
    /// Long-polling timeout passed to `getUpdates` when idle.
    pub poll_timeout_secs: u64,
    /// Base interval for draining active run streams and for the `getUpdates` error backoff.
    pub poll_backoff: Duration,
    /// Minimum interval between `editMessageText` calls for streaming answers (default 1 s).
    pub edit_interval: Duration,
    /// Optional path to persist paired user IDs and chat→bot mappings on disk.
    pub state_path: Option<PathBuf>,
    /// Optional default bot ID to use when a chat has not explicitly run `/use <bot>`.
    pub default_bot_id: Option<BotId>,
    /// Pre-paired Telegram user IDs loaded at startup.
    pub paired_users: Vec<i64>,
    /// Additional literal secret strings to register with the [`Redactor`].
    pub extra_redactor_secrets: Vec<String>,
}

impl fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("bot_token_secret", &self.bot_token_secret)
            .field("api_base_url", &self.api_base_url)
            .field("poll_timeout_secs", &self.poll_timeout_secs)
            .field("edit_interval", &self.edit_interval)
            .field("state_path", &self.state_path)
            .field("default_bot_id", &self.default_bot_id)
            .field("paired_users", &self.paired_users)
            .finish_non_exhaustive()
    }
}

impl TelegramConfig {
    /// Create a configuration reading [`TELEGRAM_BOT_TOKEN_SECRET`] from `secret_store`.
    ///
    /// # Panics
    /// Never: [`TELEGRAM_BOT_TOKEN_SECRET`] is a valid secret name.
    #[must_use]
    pub fn new(secret_store: Arc<dyn SecretStore>) -> Self {
        Self {
            secret_store,
            bot_token_secret: SecretRef::new(TELEGRAM_BOT_TOKEN_SECRET)
                .expect("default telegram secret name is valid"),
            api_base_url: DEFAULT_TELEGRAM_API_URL.to_owned(),
            poll_timeout_secs: DEFAULT_POLL_TIMEOUT_SECS,
            poll_backoff: DEFAULT_POLL_BACKOFF,
            edit_interval: DEFAULT_EDIT_INTERVAL,
            state_path: None,
            default_bot_id: None,
            paired_users: Vec::new(),
            extra_redactor_secrets: Vec::new(),
        }
    }

    /// Convenience constructor for tests: creates a [`MemoryStore`] with `bot_token` stored.
    ///
    /// # Panics
    /// Panics if `bot_token` is empty.
    #[must_use]
    pub fn with_in_memory_token(bot_token: &str) -> Self {
        let store = Arc::new(MemoryStore::new());
        store
            .set(TELEGRAM_BOT_TOKEN_SECRET, bot_token)
            .expect("storing token in memory store succeeds");
        Self::new(store)
    }

    /// Override the [`SecretRef`] used to look up the bot token.
    #[must_use]
    pub fn with_token_secret(mut self, secret_ref: SecretRef) -> Self {
        self.bot_token_secret = secret_ref;
        self
    }

    /// Override the Telegram Bot API base URL (e.g. a local `wiremock` URI).
    #[must_use]
    pub fn with_api_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api_base_url = base_url.into();
        self
    }

    /// Override the `getUpdates` long-polling timeout in seconds.
    #[must_use]
    pub fn with_poll_timeout_secs(mut self, secs: u64) -> Self {
        self.poll_timeout_secs = secs;
        self
    }

    /// Override the poll backoff duration.
    #[must_use]
    pub fn with_poll_backoff(mut self, backoff: Duration) -> Self {
        self.poll_backoff = backoff;
        self
    }

    /// Override the minimum interval between `editMessageText` calls.
    #[must_use]
    pub fn with_edit_interval(mut self, interval: Duration) -> Self {
        self.edit_interval = interval;
        self
    }

    /// Persist paired users and chat mappings to `path`.
    #[must_use]
    pub fn with_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_path = Some(path.into());
        self
    }

    /// Set the default [`BotId`] for chats that have not used `/use <bot>`.
    #[must_use]
    pub fn with_default_bot(mut self, bot_id: BotId) -> Self {
        self.default_bot_id = Some(bot_id);
        self
    }

    /// Pre-seed paired Telegram user IDs.
    #[must_use]
    pub fn with_paired_users(mut self, users: impl IntoIterator<Item = i64>) -> Self {
        self.paired_users = users.into_iter().collect();
        self
    }

    /// Register extra secret strings with the output [`Redactor`].
    #[must_use]
    pub fn with_extra_secrets(mut self, secrets: impl IntoIterator<Item = String>) -> Self {
        self.extra_redactor_secrets = secrets.into_iter().collect();
        self
    }
}

struct ActiveRun {
    telegram_chat_id: i64,
    run_id: RunId,
    stream: BoxStream<'static, Envelope>,
    editor: ThrottledEditor,
    answer_buf: String,
    tool_names: HashMap<ToolCallId, String>,
    finished: bool,
    /// Subagent run: only approvals, questions and errors are surfaced for it.
    is_child: bool,
    cancel: CancellationToken,
}

/// Result of [`TelegramChannel::attach_run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachOutcome {
    Attached,
    /// The run is already streamed (a steered message returned the same run id).
    AlreadyFollowed,
    /// `MAX_ACTIVE_RUNS` is reached; nothing was subscribed.
    Full,
}

/// A user message queued behind an active run; its own run is attached once it starts.
struct QueuedTurn {
    daemon_chat: ChatId,
    /// Unix ms when the first still-unattached message was queued.
    queued_at_ms: i64,
    queued_at: Instant,
    last_check: Option<Instant>,
    count: usize,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The part of a streamed answer that is safe to show while it is still growing.
///
/// A secret split across two deltas has no complete match yet, so its leading characters
/// would slip through redaction and stay visible in Telegram. Withhold the trailing
/// (possibly unfinished) whitespace-delimited token; the full text is sent at the end.
pub(crate) fn safe_stream_prefix(text: &str) -> &str {
    text.char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or("", |(i, c)| &text[..i + c.len_utf8()])
}

#[derive(Debug, Clone)]
struct PendingQuestion {
    telegram_chat_id: i64,
    options: Vec<String>,
}

#[derive(Default)]
struct RuntimeState {
    next_offset: Option<i64>,
    active_runs: Vec<ActiveRun>,
    run_controls: HashMap<RunId, (i64, CancellationToken)>,
    chat_active_run: HashMap<i64, RunId>,
    pending_questions: HashMap<ToolCallId, PendingQuestion>,
    chat_pending_question: HashMap<i64, ToolCallId>,
    /// Runs this bridge already attached to (never subscribe to the same run twice).
    attached_runs: HashSet<RunId>,
    /// Telegram chat -> messages queued behind a run, still waiting for their own run.
    queued: HashMap<i64, QueuedTurn>,
}

struct Inner {
    config: TelegramConfig,
    bot_api: BotApiClient,
    api_client: Arc<dyn DaemonClient>,
    pairing: PairingStore,
    redactor: Redactor,
    runtime: Mutex<RuntimeState>,
    inbox: Inbox,
    inbox_gate: AsyncMutex<()>,
    priority_gate: AsyncMutex<()>,
    drain_gate: AsyncMutex<()>,
    inbox_wake: Notify,
    priority_wake: Notify,
    clock_epoch_ms: i64,
    clock_started: Instant,
    #[cfg(test)]
    clock_offset_ms: std::sync::atomic::AtomicI64,
    /// Approval requests this bridge sent: `call_id` -> Telegram chat that received the buttons.
    /// A callback is only honoured for an entry issued to the pressing chat (anti-forgery).
    pending_approvals: Mutex<HashMap<ToolCallId, i64>>,
    /// Approvals that carry details: only "approve once" may be pressed for
    /// them, whatever callback data arrives (the daemon refuses a grant too).
    once_only_approvals: Mutex<HashSet<ToolCallId>>,
    /// Key identifying the bot the persisted polling offset belongs to.
    offset_key: String,
    cancel: CancellationToken,
    /// Wakes the run loop when a run stream was added.
    wake: Notify,
}

/// Own streams outside the runtime lock. Dropping a cancelled drain restores all
/// still-live streams; a concurrent Stop marks their tokens and they are discarded.
struct ActiveRunsLease<'a> {
    inner: &'a Inner,
    runs: Vec<ActiveRun>,
}

impl Drop for ActiveRunsLease<'_> {
    fn drop(&mut self) {
        let mut rt = self.inner.runtime.lock();
        for run in self.runs.drain(..) {
            if run.finished || run.cancel.is_cancelled() {
                rt.run_controls.remove(&run.run_id);
                if rt.chat_active_run.get(&run.telegram_chat_id) == Some(&run.run_id) {
                    rt.chat_active_run.remove(&run.telegram_chat_id);
                }
            } else {
                rt.active_runs.push(run);
            }
        }
    }
}

fn permanent_delivery_error(error: &TelegramError) -> bool {
    match error {
        TelegramError::DaemonHttp { status, .. } => (400..500).contains(status) && *status != 429,
        TelegramError::TelegramApi {
            error_code: Some(code),
            ..
        } => (400..500).contains(code) && *code != 429,
        TelegramError::Config(_) | TelegramError::Malformed(_) => true,
        _ => false,
    }
}

/// Telegram channel bridge implementing [`Channel`] and driving the long-polling loop.
#[derive(Clone)]
pub struct TelegramChannel {
    inner: Arc<Inner>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl fmt::Debug for TelegramChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramChannel")
            .field("base_url", &self.inner.bot_api.base_url())
            .field("paired_users", &self.inner.pairing.paired_users().len())
            .finish_non_exhaustive()
    }
}

fn compact_one_line(text: &str, max_chars: usize) -> String {
    let single = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= max_chars {
        single
    } else {
        let prefix: String = single.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

fn compact_json(v: &Value, max_chars: usize) -> String {
    if v.is_null() {
        return String::new();
    }
    if let Some(obj) = v.as_object()
        && obj.is_empty()
    {
        return String::new();
    }
    let raw = serde_json::to_string(v).unwrap_or_default();
    compact_one_line(&raw, max_chars)
}

/// Only `https` (or plain `http` to a loopback host, for tests) may carry the bot token.
fn validate_base_url(url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            lower.strip_prefix(p).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
            })
        });
    if lower.starts_with("https://") || loopback {
        Ok(())
    } else {
        Err(TelegramError::Config(
            "telegram api_base_url must use https (http is allowed for loopback only)".into(),
        ))
    }
}

impl TelegramChannel {
    /// Construct a [`TelegramChannel`] without spawning the background polling task.
    ///
    /// Resolves the bot token from `config.secret_store` and initialises the
    /// [`Redactor`] with all secrets currently in the store.
    ///
    /// # Errors
    /// Returns an error if the bot token is missing from `config.secret_store`,
    /// the store cannot be read, or the state file cannot be opened.
    pub fn new(config: TelegramConfig, api_client: Arc<dyn DaemonClient>) -> Result<Self> {
        validate_base_url(&config.api_base_url)?;
        let token = load_bot_token(&*config.secret_store, &config.bot_token_secret)?;
        let offset_key = token
            .expose()
            .split_once(':')
            .map_or("bot", |(id, _)| id)
            .to_owned();
        let mut redactor = Redactor::from_store(&*config.secret_store)?;
        redactor.add_secret(token.expose());
        for extra in &config.extra_redactor_secrets {
            redactor.add_secret(extra);
        }

        let pairing = match &config.state_path {
            Some(path) => PairingStore::open(path)?,
            None => PairingStore::in_memory(),
        };
        for &uid in &config.paired_users {
            pairing.add_paired_user(uid)?;
        }

        let inbox_path = config.state_path.as_ref().map(|path| {
            path.parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("telegram-inbox.jsonl")
        });
        let inbox = Inbox::open(inbox_path, &offset_key)?;
        let bot_api = BotApiClient::with_base_url(token, &config.api_base_url);
        let runtime = RuntimeState {
            next_offset: pairing.saved_offset(&offset_key),
            ..RuntimeState::default()
        };
        let inner = Arc::new(Inner {
            config,
            bot_api,
            api_client,
            pairing,
            redactor,
            runtime: Mutex::new(runtime),
            inbox,
            inbox_gate: AsyncMutex::new(()),
            priority_gate: AsyncMutex::new(()),
            drain_gate: AsyncMutex::new(()),
            inbox_wake: Notify::new(),
            priority_wake: Notify::new(),
            clock_epoch_ms: now_ms(),
            clock_started: Instant::now(),
            #[cfg(test)]
            clock_offset_ms: std::sync::atomic::AtomicI64::new(0),
            pending_approvals: Mutex::new(HashMap::new()),
            once_only_approvals: Mutex::new(HashSet::new()),
            offset_key,
            cancel: CancellationToken::new(),
            wake: Notify::new(),
        });

        Ok(Self {
            inner,
            task: Arc::new(Mutex::new(None)),
        })
    }

    /// Start the Telegram channel bridge in a background Tokio task.
    ///
    /// # Errors
    /// Returns an error if resolving the bot token or opening the state store fails.
    pub fn start(config: TelegramConfig, api_client: Arc<dyn DaemonClient>) -> Result<Self> {
        let channel = Self::new(config, api_client)?;
        let worker = channel.clone();
        let handle = tokio::spawn(async move {
            worker.run_loop().await;
        });
        *channel.task.lock() = Some(handle);
        Ok(channel)
    }

    /// Underlying [`PairingStore`].
    #[must_use]
    pub fn pairing(&self) -> &PairingStore {
        &self.inner.pairing
    }

    /// Underlying [`BotApiClient`].
    #[must_use]
    pub fn bot_api(&self) -> &BotApiClient {
        &self.inner.bot_api
    }

    /// Generate a new one-time 6-digit pairing code valid for 10 minutes ([`crate::PAIR_CODE_TTL`]).
    #[must_use]
    pub fn create_pair_code(&self) -> String {
        self.inner.pairing.create_code()
    }

    /// Generate a new one-time 6-digit pairing code with a custom validity duration.
    #[must_use]
    pub fn create_pair_code_with_ttl(&self, ttl: Duration) -> String {
        self.inner.pairing.create_code_with_ttl(ttl)
    }

    /// Whether the given Telegram `user_id` is paired.
    #[must_use]
    pub fn is_paired(&self, user_id: i64) -> bool {
        self.inner.pairing.is_paired(user_id)
    }

    /// Redact `text` through the channel's [`Redactor`].
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        self.inner.redactor.redact(text)
    }

    fn redact_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.redact(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.redact_value(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (self.redact(k), self.redact_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Signal the background polling loop to stop.
    pub fn stop(&self) {
        self.inner.cancel.cancel();
    }

    /// Stop the background polling loop and wait for it to exit.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let handle = self.task.lock().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    fn inbox_now_ms(&self) -> i64 {
        let now = self.inner.clock_epoch_ms.saturating_add(
            i64::try_from(self.inner.clock_started.elapsed().as_millis()).unwrap_or(i64::MAX),
        );
        #[cfg(test)]
        let now = now.saturating_add(
            self.inner
                .clock_offset_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        now
    }

    #[cfg(test)]
    pub(crate) fn advance_retry_clock(&self, duration: Duration) {
        self.inner.clock_offset_ms.fetch_add(
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.inner.inbox_wake.notify_one();
        self.inner.priority_wake.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn fail_inbox_writes(&self, enabled: bool) {
        self.inner.inbox.fail_writes(enabled);
    }

    fn report_error(&self, error: &TelegramError) {
        eprintln!("telegram: {}", self.redact(&error.to_string()));
    }

    /// Take durable ownership of the entire batch before acknowledging any update.
    fn handle_updates(&self, updates: &[Update]) -> Result<usize> {
        self.inner.inbox.enqueue(updates, self.inbox_now_ms())?;
        let saved = if let Some(id) = updates.iter().map(|u| u.update_id).max() {
            self.advance_offset(id)
        } else {
            Ok(())
        };
        // Offset persistence can fail after a successful inbox fsync. Delivery still
        // proceeds from the inbox, and a restart safely deduplicates Telegram replays.
        self.inner.inbox_wake.notify_one();
        self.inner.priority_wake.notify_one();
        saved.map(|()| updates.len())
    }

    /// Polling, ordered delivery, priority Stop, and outbound streaming have separate
    /// futures. A slow Telegram send or inline 429 retry cannot suspend `getUpdates`
    /// or cancellation. All four futures terminate with the channel's token.
    async fn run_loop(&self) {
        tokio::join!(
            biased;
            self.poll_loop(),
            self.inbox_loop(true),
            self.inbox_loop(false),
            self.drain_loop(),
        );
    }

    async fn poll_loop(&self) {
        let base = self.inner.config.poll_backoff;
        let mut failures = 0u32;
        let mut delay = Duration::ZERO;
        loop {
            tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            let offset = self.inner.runtime.lock().next_offset;
            let polled = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                result = self.inner.bot_api.get_updates(offset, self.inner.config.poll_timeout_secs) => result,
            };
            let result = match polled {
                Ok(updates) => self.handle_updates(&updates),
                Err(error) => Err(error),
            };
            match result {
                Ok(count) => {
                    failures = 0;
                    delay = if count == 0 { base } else { Duration::ZERO };
                }
                Err(error) => {
                    self.report_error(&error);
                    failures = failures.saturating_add(1);
                    delay = poll_error_delay(&error, failures, base);
                }
            }
        }
    }

    async fn inbox_loop(&self, priority: bool) {
        let wake = if priority {
            &self.inner.priority_wake
        } else {
            &self.inner.inbox_wake
        };
        loop {
            let result = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                result = self.process_pending(priority) => result,
            };
            if let Err(error) = result {
                self.report_error(&error);
            }
            let delay = self
                .inner
                .inbox
                .retry_delay(priority, self.inbox_now_ms())
                .unwrap_or(Duration::from_secs(3600))
                .max(self.inner.config.poll_backoff);
            tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                () = wake.notified() => {},
                () = tokio::time::sleep(delay) => {},
            }
        }
    }

    async fn process_pending(&self, priority: bool) -> Result<usize> {
        let gate = if priority {
            &self.inner.priority_gate
        } else {
            &self.inner.inbox_gate
        };
        let _guard = gate.lock().await;
        let mut processed = 0;
        let mut first_error = None;
        while processed < MAX_INBOX_PER_PASS {
            let Some(entry) = self.inner.inbox.next(priority, self.inbox_now_ms()) else {
                break;
            };
            if entry.attempts >= MAX_ATTEMPTS && !entry.delivered {
                self.dead_letter(entry.update_id).await?;
                processed += 1;
                continue;
            }
            self.inner
                .inbox
                .begin_attempt(entry.update_id, self.inbox_now_ms())?;
            let delivery = async {
                if entry.delivered && entry.delivery.is_none() {
                    return Ok(());
                }
                match entry.payload.into_incoming() {
                    Some(incoming) => self.handle_incoming(incoming).await,
                    None => Ok(()),
                }
            };
            let result = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return Ok(processed),
                result = delivery => result,
            };
            processed += 1;
            match result {
                Ok(()) => self.inner.inbox.done(entry.update_id)?,
                Err(error) => {
                    let terminal = permanent_delivery_error(&error)
                        || entry.attempts.saturating_add(1) >= MAX_ATTEMPTS;
                    first_error.get_or_insert(error);
                    if terminal {
                        self.dead_letter(entry.update_id).await?;
                    } else {
                        // Preserve delivery order: later normal messages wait behind a
                        // transient failure. The independent /stop worker is never blocked.
                        break;
                    }
                }
            }
        }
        first_error.map_or(Ok(processed), Err)
    }

    async fn dead_letter(&self, id: i64) -> Result<()> {
        if let Some(chat_id) = self.inner.inbox.dead(id)? {
            let notice = format!("Could not process message {id}.");
            tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => {},
                result = self.send_redacted(chat_id, &notice, None) => {
                    if let Err(error) = result { self.report_error(&error); }
                }
            }
        }
        Ok(())
    }

    async fn drain_loop(&self) {
        let base = self.inner.config.poll_backoff;
        let mut interval = base;
        loop {
            let needs_drain = {
                let rt = self.inner.runtime.lock();
                !rt.active_runs.is_empty() || !rt.queued.is_empty()
            };
            let wait = if needs_drain {
                interval
            } else {
                Duration::from_secs(3600)
            };
            tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                () = self.inner.wake.notified() => {},
                () = tokio::time::sleep(wait) => {},
            }
            let result = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                result = self.drain_active_runs() => result,
            };
            interval = match result {
                Ok(0) => (interval * 2).min(MAX_IDLE_DRAIN_INTERVAL),
                Ok(_) => base,
                Err(error) => {
                    self.report_error(&error);
                    poll_error_delay(&error, 1, base)
                }
            };
        }
    }

    /// Only called after the corresponding batch has been fsynced to the inbox.
    fn advance_offset(&self, update_id: i64) -> Result<()> {
        let next = update_id.saturating_add(1);
        let mut rt = self.inner.runtime.lock();
        let merged = rt.next_offset.map_or(next, |current| current.max(next));
        let result = self
            .inner
            .pairing
            .save_offset(&self.inner.offset_key, merged);
        // Even if the pairing file cannot be saved, the inbox already owns delivery.
        // On restart an older offset causes harmless deduplicated replays.
        rt.next_offset = Some(merged);
        result
    }

    /// Poll once, durably record updates, process ready inbox items, then drain a
    /// bounded number of events. Retry delays are honoured across calls and restarts.
    ///
    /// # Errors
    /// Returns an error for an inbox/offset write or Telegram/daemon API failure.
    pub async fn poll_once(&self) -> Result<usize> {
        let (offset, timeout) = {
            let rt = self.inner.runtime.lock();
            (
                rt.next_offset,
                if rt.run_controls.is_empty() {
                    self.inner.config.poll_timeout_secs
                } else {
                    0
                },
            )
        };
        let updates = self.inner.bot_api.get_updates(offset, timeout).await?;
        let count = updates.len();
        let mut first_error = self.handle_updates(&updates).err();
        for priority in [true, false] {
            if let Err(error) = self.process_pending(priority).await {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.drain_active_runs().await {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(count), Err)
    }

    /// Durably process one update and drain a bounded number of run events.
    /// Duplicate update IDs do not repeat a daemon side effect.
    ///
    /// # Errors
    /// Returns an error for an inbox/offset write or Telegram/daemon API failure.
    pub async fn handle_update(&self, update: Update) -> Result<()> {
        self.handle_updates(&[update])?;
        let mut first_error = None;
        for priority in [true, false] {
            if let Err(error) = self.process_pending(priority).await {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.drain_active_runs().await {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Show an approval request. Details a tool attaches (a schedule) are shown
    /// in full before approval is offered; if that is impossible, approval is
    /// left to the desktop app. The buttons go on the last message only.
    async fn send_approval(
        &self,
        chat_id: i64,
        is_child: bool,
        a: protocol::event::ApprovalNeeded,
    ) -> Result<()> {
        let run_is_child = is_child;
        let prefix = if run_is_child { "subagent: " } else { "" };
        // A tool that attaches details (a schedule) must be shown in
        // full before anyone can approve it here; if that is not
        // possible, approval is left to the desktop app.
        let (body, offer) = match &a.details {
            None => (
                format!(
                    "⚠️ Approval needed ({prefix}{})
{}",
                    a.tool_name, a.summary
                ),
                true,
            ),
            Some(d) => match protocol::approval_text::describe_details(d) {
                Some(full) => (
                    format!(
                        "⚠️ Approval needed ({prefix}{})
{full}",
                        a.tool_name
                    ),
                    true,
                ),
                None => (
                    format!(
                        "⚠️ {} needs approval, but this chat cannot show what it would do. Approve or deny it in the desktop app.",
                        a.tool_name
                    ),
                    false,
                ),
            },
        };
        let markup = if offer {
            {
                let mut pending = self.inner.pending_approvals.lock();
                if pending.len() >= MAX_PENDING_PROMPTS {
                    pending.clear();
                }
                pending.insert(a.call_id, chat_id);
            }
            if a.details.is_some() {
                let mut once = self.inner.once_only_approvals.lock();
                if once.len() >= MAX_PENDING_PROMPTS {
                    once.clear();
                }
                once.insert(a.call_id);
            }
            let mut row = vec![InlineButton::new(
                "Approve once",
                format_approval_callback(a.call_id, "once"),
            )];
            // No standing grant for tools that bring details.
            if a.details.is_none() {
                row.push(InlineButton::new(
                    "Always",
                    format_approval_callback(a.call_id, "always"),
                ));
            }
            row.push(InlineButton::new(
                "Deny",
                format_approval_callback(a.call_id, "deny"),
            ));
            InlineKeyboardMarkup::from_rows(vec![row])
        } else {
            None
        };
        // Redact first, then split, so a secret cannot straddle two
        // messages; the buttons go on the last message only.
        // Telegram counts UTF-16 units.
        let chunks = protocol::approval_text::chunk_text(&self.redact(&body), 3500);
        let last = chunks.len() - 1;
        for (i, chunk) in chunks.iter().enumerate() {
            let m = if i == last { markup.as_ref() } else { None };
            if let Err(e) = self.send_redacted(chat_id, chunk, m).await {
                // Never leave a button that approves something the user did
                // not get to read in full: forget this approval here (it stays
                // answerable from the desktop app) and say so.
                self.inner.pending_approvals.lock().remove(&a.call_id);
                self.inner.once_only_approvals.lock().remove(&a.call_id);
                eprintln!(
                    "telegram: approval {} for {} not offered here: message {} of {} failed: {e}",
                    a.call_id,
                    a.tool_name,
                    i + 1,
                    chunks.len()
                );
                let _ = self
                    .send_redacted(
                        chat_id,
                        "Approval could not be shown completely here. Approve or deny it in the desktop app.",
                        None,
                    )
                    .await;
                return Ok(());
            }
        }
        Ok(())
    }

    async fn send_redacted(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64> {
        let clean = self.redact(text);
        let msg = self
            .inner
            .bot_api
            .send_message(chat_id, &clean, reply_markup)
            .await?;
        Ok(msg.message_id)
    }

    async fn handle_incoming(&self, msg: IncomingMessage) -> Result<()> {
        let chat_id = msg.chat.0;
        let user_id = msg.user_id;

        if let IncomingKind::Text(ref text) = msg.kind {
            let trimmed = text.trim();
            if trimmed == "/pair" || trimmed.starts_with("/pair ") || trimmed.starts_with("/pair@")
            {
                return self.handle_pair_command(chat_id, user_id, trimmed).await;
            }
        }

        // Security gate: only paired Telegram user IDs may interact with the bot.
        if !self.inner.pairing.is_paired(user_id) {
            if self.inner.pairing.should_send_refusal(user_id) {
                // Best effort: a failing refusal (blocked bot, 429) is never fatal.
                match msg.kind {
                    IncomingKind::Text(_) => {
                        let _ = self
                            .send_redacted(chat_id, UNPAIRED_REFUSAL_TEXT, None)
                            .await;
                    }
                    IncomingKind::Callback { ref query_id, .. } => {
                        let _ = self
                            .inner
                            .bot_api
                            .answer_callback_query(query_id, Some(UNPAIRED_REFUSAL_TEXT))
                            .await;
                    }
                }
            }
            return Ok(());
        }

        match msg.kind {
            IncomingKind::Text(text) => {
                self.handle_paired_text(chat_id, &text, msg.update_id).await
            }
            IncomingKind::Callback { query_id, data, .. } => {
                self.handle_callback(chat_id, &query_id, &data, msg.update_id)
                    .await
            }
        }
    }

    async fn handle_pair_command(&self, chat_id: i64, user_id: i64, raw: &str) -> Result<()> {
        let code = raw
            .split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest.trim());
        if code.is_empty() {
            self.send_redacted(chat_id, "Usage: /pair <6-digit-code>", None)
                .await?;
            return Ok(());
        }

        match self.inner.pairing.redeem_code(user_id, code)? {
            PairAttempt::Paired => {
                self.send_redacted(
                    chat_id,
                    "✅ Paired successfully! Send a message or use /bots to choose a bot.",
                    None,
                )
                .await?;
            }
            PairAttempt::AlreadyPaired => {
                self.send_redacted(chat_id, "✅ Your Telegram account is already paired.", None)
                    .await?;
            }
            PairAttempt::Expired => {
                self.send_redacted(
                    chat_id,
                    "❌ Pairing code has expired. Generate a new code and try again.",
                    None,
                )
                .await?;
            }
            PairAttempt::InvalidCode => {
                self.send_redacted(chat_id, "❌ Invalid pairing code.", None)
                    .await?;
            }
            // Locked-out guesser: stay silent.
            PairAttempt::RateLimited => {}
        }
        Ok(())
    }

    async fn handle_paired_text(&self, chat_id: i64, text: &str, update_id: i64) -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        if trimmed.chars().count() > MAX_INCOMING_CHARS {
            self.send_redacted(chat_id, "Message too long; please shorten it.", None)
                .await?;
            return Ok(());
        }

        if trimmed == "/bots" || trimmed.starts_with("/bots@") {
            return self.handle_bots_command(chat_id).await;
        }
        if trimmed == "/use" || trimmed.starts_with("/use ") || trimmed.starts_with("/use@") {
            let arg = trimmed
                .split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest.trim());
            return self.handle_use_command(chat_id, arg).await;
        }
        if trimmed == "/stop" || trimmed.starts_with("/stop@") {
            return self.handle_stop_command(chat_id, update_id).await;
        }
        if trimmed == "/status" || trimmed.starts_with("/status@") {
            return self.handle_status_command(chat_id).await;
        }

        // A retry after daemon acknowledgement resumes the original delivery. A
        // question arriving in the meantime must not reinterpret the accepted text.
        if self
            .inner
            .inbox
            .get(update_id)
            .is_some_and(|entry| entry.delivery.is_some())
        {
            return self.forward_user_message(chat_id, trimmed, update_id).await;
        }

        // If the chat has a pending `ask_user` question, free text answers it.
        let pending_q = self
            .inner
            .runtime
            .lock()
            .chat_pending_question
            .get(&chat_id)
            .copied();
        if let Some(call_id) = pending_q {
            self.inner
                .api_client
                .answer_question(call_id, trimmed)
                .await?;
            self.inner.inbox.delivered(update_id, None)?;
            let mut rt = self.inner.runtime.lock();
            rt.chat_pending_question.remove(&chat_id);
            rt.pending_questions.remove(&call_id);
            return Ok(());
        }

        self.forward_user_message(chat_id, trimmed, update_id).await
    }

    async fn handle_bots_command(&self, chat_id: i64) -> Result<()> {
        let bots = self.inner.api_client.list_bots().await?;
        if bots.is_empty() {
            self.send_redacted(chat_id, "No bots configured.", None)
                .await?;
            return Ok(());
        }
        let active = self
            .inner
            .pairing
            .bot_for_chat(chat_id)
            .or(self.inner.config.default_bot_id)
            .or_else(|| bots.first().map(|b| b.id));

        let mut lines = Vec::with_capacity(bots.len() + 1);
        lines.push("Available bots:".to_owned());
        for b in &bots {
            let mark = if Some(b.id) == active { "*" } else { "•" };
            lines.push(format!("{mark} {} ({})", b.name, b.id));
        }
        self.send_redacted(chat_id, &lines.join("\n"), None).await?;
        Ok(())
    }

    async fn handle_use_command(&self, chat_id: i64, query: &str) -> Result<()> {
        if query.is_empty() {
            self.send_redacted(chat_id, "Usage: /use <bot-name-or-id>", None)
                .await?;
            return Ok(());
        }
        let bots = self.inner.api_client.list_bots().await?;
        let needle = query.to_lowercase();
        let found = bots
            .iter()
            .find(|b| b.name.to_lowercase() == needle || b.id.to_string() == needle)
            .or_else(|| {
                bots.iter().find(|b| {
                    b.name.to_lowercase().contains(&needle) || b.id.to_string().starts_with(&needle)
                })
            });

        let Some(bot) = found else {
            self.send_redacted(
                chat_id,
                &format!("Bot {query:?} not found. Use /bots to list available bots."),
                None,
            )
            .await?;
            return Ok(());
        };

        self.inner.pairing.set_bot_for_chat(chat_id, bot.id)?;
        self.send_redacted(
            chat_id,
            &format!("Switched active bot to {} ({}).", bot.name, bot.id),
            None,
        )
        .await?;
        Ok(())
    }

    async fn handle_stop_command(&self, chat_id: i64, update_id: i64) -> Result<()> {
        let saved_target = self
            .inner
            .inbox
            .get(update_id)
            .filter(|entry| entry.stop_chat_id.is_some())
            .map(|entry| entry.cancel_run);
        let active_run = saved_target.unwrap_or_else(|| {
            self.inner
                .runtime
                .lock()
                .chat_active_run
                .get(&chat_id)
                .copied()
        });
        // Persist the cancellation target before removing it from live state. A failed
        // daemon cancellation can then retry the same run, including after a restart.
        self.inner.inbox.stop(update_id, chat_id, active_run)?;
        {
            let mut rt = self.inner.runtime.lock();
            rt.chat_active_run.remove(&chat_id);
            rt.run_controls.retain(|_, (chat, cancel)| {
                if *chat == chat_id {
                    cancel.cancel();
                    false
                } else {
                    true
                }
            });
            rt.active_runs.retain(|r| r.telegram_chat_id != chat_id);
            rt.queued.remove(&chat_id);
        }

        if let Some(run_id) = active_run {
            self.inner.api_client.cancel_run(run_id).await?;
            self.inner.inbox.delivered(update_id, None)?;
            self.send_redacted(chat_id, &format!("🛑 Stopped run {run_id}."), None)
                .await?;
        } else {
            self.inner.inbox.delivered(update_id, None)?;
            self.send_redacted(chat_id, "No active run to stop.", None)
                .await?;
        }
        Ok(())
    }

    async fn handle_status_command(&self, chat_id: i64) -> Result<()> {
        let st = self.inner.api_client.status().await?;
        let active_bot = self
            .inner
            .pairing
            .bot_for_chat(chat_id)
            .or(self.inner.config.default_bot_id);
        let active_run = {
            let rt = self.inner.runtime.lock();
            rt.chat_active_run.get(&chat_id).copied()
        };

        let bot_line = active_bot.map_or_else(|| "default".to_owned(), |id| id.to_string());
        let run_line = active_run.map_or_else(|| "idle".to_owned(), |id| id.to_string());
        let msg = format!(
            "Daemon: {} (v{}, up {}s)\nPending approvals: {}\nBot: {}\nRun: {}",
            st.status, st.version, st.uptime_secs, st.pending_approvals, bot_line, run_line
        );
        self.send_redacted(chat_id, &msg, None).await?;
        Ok(())
    }

    async fn resolve_bot_for_chat(&self, chat_id: i64) -> Result<BotId> {
        if let Some(bot_id) = self.inner.pairing.bot_for_chat(chat_id) {
            return Ok(bot_id);
        }
        if let Some(bot_id) = self.inner.config.default_bot_id {
            self.inner.pairing.set_bot_for_chat(chat_id, bot_id)?;
            return Ok(bot_id);
        }
        let bots = self.inner.api_client.list_bots().await?;
        let first = bots
            .first()
            .ok_or_else(|| TelegramError::Daemon("no bots configured on the daemon".into()))?;
        self.inner.pairing.set_bot_for_chat(chat_id, first.id)?;
        Ok(first.id)
    }

    async fn resolve_daemon_chat(&self, telegram_chat_id: i64, bot_id: BotId) -> Result<ChatId> {
        if let Some(chat_id) = self.inner.pairing.thread_for(telegram_chat_id, bot_id) {
            return Ok(chat_id);
        }
        let title = format!("telegram:{telegram_chat_id}");
        let created = self
            .inner
            .api_client
            .create_chat(bot_id, Some(&title))
            .await?;
        self.inner
            .pairing
            .set_thread_for(telegram_chat_id, bot_id, created.id)?;
        Ok(created.id)
    }

    async fn deliver_user_message(
        &self,
        telegram_chat_id: i64,
        text: &str,
        update_id: i64,
    ) -> Result<Option<DeliveryReceipt>> {
        if self.inner.inbox.cancelled(update_id, telegram_chat_id)
            && self
                .inner
                .inbox
                .get(update_id)
                .is_none_or(|entry| !entry.delivered)
        {
            // A priority Stop may arrive in the same poll batch as a not-yet-started
            // message. Honour that cancellation instead of starting it afterwards.
            return Ok(None);
        }
        let receipt = if let Some(receipt) = self
            .inner
            .inbox
            .get(update_id)
            .and_then(|entry| entry.delivery)
        {
            receipt
        } else {
            let bot_id = match self.resolve_bot_for_chat(telegram_chat_id).await {
                Ok(id) => id,
                Err(TelegramError::Daemon(message)) if message.contains("no bots configured") => {
                    self.send_redacted(
                        telegram_chat_id,
                        "No bots configured on the daemon. Add a bot first.",
                        None,
                    )
                    .await?;
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let daemon_chat = self.resolve_daemon_chat(telegram_chat_id, bot_id).await?;
            let sent_at_ms = now_ms();
            let outcome = self
                .inner
                .api_client
                .send_message_idempotent(daemon_chat, text, &format!("tg:{update_id}"))
                .await?;
            let receipt = DeliveryReceipt {
                daemon_chat,
                outcome,
                sent_at_ms,
            };
            // Commit the daemon acknowledgement BEFORE subscription or Telegram output.
            // The daemon must honour the key to cover a crash before this local fsync.
            self.inner
                .inbox
                .delivered(update_id, Some(receipt.clone()))?;
            receipt
        };
        Ok(Some(receipt))
    }

    async fn forward_user_message(
        &self,
        telegram_chat_id: i64,
        text: &str,
        update_id: i64,
    ) -> Result<()> {
        let Some(receipt) = self
            .deliver_user_message(telegram_chat_id, text, update_id)
            .await?
        else {
            return Ok(());
        };
        let DeliveryReceipt {
            daemon_chat: daemon_chat_id,
            outcome,
            sent_at_ms,
        } = receipt;

        if self.inner.inbox.cancelled(update_id, telegram_chat_id) {
            if let Some(run_id) = outcome.run_id {
                self.inner.api_client.cancel_run(run_id).await?;
            }
            self.inner.inbox.delivered(update_id, None)?;
            return Ok(());
        }

        match outcome.delivery {
            DeliveryKind::Stopped => {
                {
                    let mut rt = self.inner.runtime.lock();
                    if let Some(rid) = outcome
                        .run_id
                        .or_else(|| rt.chat_active_run.remove(&telegram_chat_id))
                    {
                        rt.chat_active_run.remove(&telegram_chat_id);
                        if let Some((_, cancel)) = rt.run_controls.remove(&rid) {
                            cancel.cancel();
                        }
                        rt.active_runs.retain(|r| r.run_id != rid);
                    }
                    rt.queued.remove(&telegram_chat_id);
                }
                self.send_redacted(telegram_chat_id, "🛑 Run stopped.", None)
                    .await?;
            }
            DeliveryKind::Queued if outcome.run_id.is_none() => {
                // The daemon starts this turn later as a brand-new run nobody subscribed to;
                // remember it so `attach_queued_runs` can find and follow that run.
                {
                    let mut rt = self.inner.runtime.lock();
                    let entry = rt
                        .queued
                        .entry(telegram_chat_id)
                        .or_insert_with(|| QueuedTurn {
                            daemon_chat: daemon_chat_id,
                            queued_at_ms: sent_at_ms,
                            queued_at: Instant::now(),
                            last_check: None,
                            count: 0,
                        });
                    entry.daemon_chat = daemon_chat_id;
                    entry.count += 1;
                }
                self.inner.inbox.delivered(update_id, None)?;
                self.inner.wake.notify_one();
                self.send_redacted(
                    telegram_chat_id,
                    "⏳ Message queued behind the active run.",
                    None,
                )
                .await?;
            }
            _ => {
                if let Some(run_id) = outcome.run_id
                    && self
                        .attach_run(telegram_chat_id, run_id, false, None)
                        .await?
                        == AttachOutcome::Full
                {
                    self.send_redacted(
                        telegram_chat_id,
                        "⚠️ Too many runs are being followed right now; this run's output will not be streamed here. Try again in a moment.",
                        None,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Subscribe to `run_id` and start streaming it into `telegram_chat_id`.
    ///
    /// Does nothing if the run is already followed (a steered message returns the
    /// already-attached run id) or the run table is full; see [`AttachOutcome`].
    async fn attach_run(
        &self,
        telegram_chat_id: i64,
        run_id: RunId,
        is_child: bool,
        parent_cancel: Option<CancellationToken>,
    ) -> Result<AttachOutcome> {
        let cancel =
            parent_cancel.map_or_else(CancellationToken::new, |parent| parent.child_token());
        {
            let mut rt = self.inner.runtime.lock();
            if rt.attached_runs.contains(&run_id) {
                return Ok(AttachOutcome::AlreadyFollowed);
            }
            if rt.run_controls.len() >= MAX_ACTIVE_RUNS {
                return Ok(AttachOutcome::Full);
            }
            if rt.attached_runs.len() >= MAX_ATTACHED_RUNS {
                rt.attached_runs.clear();
            }
            rt.attached_runs.insert(run_id);
            rt.run_controls
                .insert(run_id, (telegram_chat_id, cancel.clone()));
            if !is_child {
                rt.chat_active_run.insert(telegram_chat_id, run_id);
            }
        }
        let subscribed = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => None,
            () = cancel.cancelled() => None,
            result = self.inner.api_client.subscribe_run(run_id) => Some(result),
        };
        let stream = match subscribed {
            Some(Ok(stream)) if !cancel.is_cancelled() => stream,
            result => {
                let mut rt = self.inner.runtime.lock();
                rt.attached_runs.remove(&run_id);
                rt.run_controls.remove(&run_id);
                if rt.chat_active_run.get(&telegram_chat_id) == Some(&run_id) {
                    rt.chat_active_run.remove(&telegram_chat_id);
                }
                return match result {
                    Some(Err(error)) => Err(error),
                    _ => Ok(AttachOutcome::AlreadyFollowed),
                };
            }
        };
        {
            let mut rt = self.inner.runtime.lock();
            if cancel.is_cancelled() {
                return Ok(AttachOutcome::AlreadyFollowed);
            }
            rt.active_runs.push(ActiveRun {
                telegram_chat_id,
                run_id,
                stream: stream.fuse().boxed(),
                editor: ThrottledEditor::new(telegram_chat_id, self.inner.config.edit_interval),
                answer_buf: String::new(),
                tool_names: HashMap::new(),
                finished: false,
                is_child,
                cancel,
            });
        }
        self.inner.wake.notify_one();
        Ok(AttachOutcome::Attached)
    }

    /// Follow runs the daemon started for messages that were queued behind another run.
    async fn attach_queued_runs(&self) -> Result<()> {
        let candidates: Vec<(i64, ChatId, i64, Vec<RunId>)> = {
            let mut rt = self.inner.runtime.lock();
            rt.queued.retain(|_, q| q.queued_at.elapsed() < QUEUED_TTL);
            let busy: HashSet<i64> = rt.chat_active_run.keys().copied().collect();
            let exclude: Vec<RunId> = rt.attached_runs.iter().copied().collect();
            let now = Instant::now();
            rt.queued
                .iter_mut()
                .filter(|(chat, q)| {
                    !busy.contains(chat)
                        && q.last_check
                            .is_none_or(|t| now.duration_since(t) >= QUEUED_CHECK_INTERVAL)
                })
                .map(|(chat, q)| {
                    q.last_check = Some(now);
                    (*chat, q.daemon_chat, q.queued_at_ms, exclude.clone())
                })
                .collect()
        };
        let mut first_err: Option<TelegramError> = None;
        for (tg_chat, daemon_chat, since_ms, exclude) in candidates {
            let found = match self
                .inner
                .api_client
                .find_chat_run(daemon_chat, &exclude, since_ms)
                .await
            {
                Ok(f) => f,
                Err(e) => {
                    first_err.get_or_insert(e);
                    continue;
                }
            };
            let Some(run_id) = found else { continue };
            if !self
                .inner
                .runtime
                .lock()
                .queued
                .get(&tg_chat)
                .is_some_and(|q| q.daemon_chat == daemon_chat && q.queued_at_ms == since_ms)
            {
                continue;
            }
            match self.attach_run(tg_chat, run_id, false, None).await {
                // Run table full: keep the entry and retry on the next pass.
                Ok(AttachOutcome::Full) => {}
                Ok(_) => {
                    let mut rt = self.inner.runtime.lock();
                    if let Some(q) = rt.queued.get_mut(&tg_chat) {
                        q.count = q.count.saturating_sub(1);
                        q.queued_at_ms = now_ms();
                        if q.count == 0 {
                            rt.queued.remove(&tg_chat);
                        }
                    }
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    async fn handle_callback(
        &self,
        chat_id: i64,
        query_id: &str,
        data: &str,
        update_id: i64,
    ) -> Result<()> {
        if let Some((call_id, decision, scope)) = parse_approval_callback(data) {
            // Only approvals this bridge issued to *this* chat may be decided from it.
            let allowed = { self.inner.pending_approvals.lock().get(&call_id) == Some(&chat_id) };
            if !allowed {
                self.inner
                    .bot_api
                    .answer_callback_query(query_id, Some("Unknown or expired request"))
                    .await?;
                return Ok(());
            }
            // A card with details is approved once, never as a standing grant.
            let once_only = self.inner.once_only_approvals.lock().contains(&call_id);
            if once_only
                && decision == ApprovalDecision::Approve
                && scope != crate::daemon_client::ApprovalScope::Once
            {
                self.inner
                    .bot_api
                    .answer_callback_query(query_id, Some("This can only be approved once"))
                    .await?;
                return Ok(());
            }
            if let Err(e) = self
                .inner
                .api_client
                .decide_approval(call_id, decision, scope, None)
                .await
            {
                // Let the user retry the button.
                self.inner.pending_approvals.lock().insert(call_id, chat_id);
                return Err(e);
            }
            self.inner.inbox.delivered(update_id, None)?;
            self.inner.pending_approvals.lock().remove(&call_id);
            self.inner.once_only_approvals.lock().remove(&call_id);
            let ack = match decision {
                ApprovalDecision::Approve => "Approved",
                ApprovalDecision::Deny => "Denied",
            };
            self.inner
                .bot_api
                .answer_callback_query(query_id, Some(ack))
                .await?;
            return Ok(());
        }

        if let Some((call_id, payload)) = parse_question_callback(data) {
            let answer = {
                let rt = self.inner.runtime.lock();
                // Only questions this bridge asked in *this* chat can be answered from it,
                // and only with one of the offered options.
                rt.pending_questions.get(&call_id).and_then(|pq| {
                    (pq.telegram_chat_id == chat_id)
                        .then(|| {
                            payload
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| pq.options.get(i))
                        })
                        .flatten()
                        .cloned()
                })
            };
            let Some(answer) = answer else {
                self.inner
                    .bot_api
                    .answer_callback_query(query_id, Some("Unknown or expired request"))
                    .await?;
                return Ok(());
            };
            self.inner
                .api_client
                .answer_question(call_id, &answer)
                .await?;
            self.inner.inbox.delivered(update_id, None)?;
            {
                let mut rt = self.inner.runtime.lock();
                rt.pending_questions.remove(&call_id);
                if rt.chat_pending_question.get(&chat_id) == Some(&call_id) {
                    rt.chat_pending_question.remove(&chat_id);
                }
            }
            self.inner
                .bot_api
                .answer_callback_query(query_id, Some("Answered"))
                .await?;
            return Ok(());
        }

        self.inner
            .bot_api
            .answer_callback_query(query_id, None)
            .await?;
        Ok(())
    }

    /// Drain a bounded pass. No runtime lock is held while awaiting a stream,
    /// Telegram HTTP, retries, final-edit throttles, or daemon subscriptions.
    async fn drain_active_runs(&self) -> Result<usize> {
        let _guard = self.inner.drain_gate.lock().await;
        let runs = std::mem::take(&mut self.inner.runtime.lock().active_runs);
        let mut lease = ActiveRunsLease {
            inner: &self.inner,
            runs,
        };
        let mut first_error = None;
        let mut events = 0;
        let mut spawned = Vec::new();
        let deadline = Instant::now() + MAX_DRAIN_TIME;
        let mut visited = 0;
        for run in &mut lease.runs {
            if events >= MAX_DRAIN_EVENTS || Instant::now() >= deadline {
                break;
            }
            visited += 1;
            if run.finished || run.cancel.is_cancelled() {
                continue;
            }
            let cancel = run.cancel.clone();
            let result = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => break,
                () = cancel.cancelled() => continue,
                result = self.drain_one_run(run, &mut spawned, &mut events, deadline) => result,
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        // Fairness between runs when one pass hits its global event/time budget.
        if !lease.runs.is_empty() {
            let rotation = visited % lease.runs.len();
            lease.runs.rotate_left(rotation);
        }
        for (chat_id, child, parent_cancel) in spawned {
            if !parent_cancel.is_cancelled()
                && let Err(error) = self
                    .attach_run(chat_id, child, true, Some(parent_cancel))
                    .await
            {
                first_error.get_or_insert(error);
            }
        }
        drop(lease);
        if let Err(error) = self.attach_queued_runs().await {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(events), Err)
    }

    async fn drain_one_run(
        &self,
        run: &mut ActiveRun,
        spawned: &mut Vec<(i64, RunId, CancellationToken)>,
        events: &mut usize,
        deadline: Instant,
    ) -> Result<()> {
        let mut per_run = 0;
        while per_run < MAX_EVENTS_PER_RUN
            && *events < MAX_DRAIN_EVENTS
            && Instant::now() < deadline
        {
            let Some(next_env) = run.stream.next().now_or_never() else {
                run.editor.flush_if_due(&self.inner.bot_api).await?;
                return Ok(());
            };
            let Some(env) = next_env else {
                return self.finish_dropped_run(run).await;
            };
            per_run += 1;
            *events += 1;
            self.process_run_event(run, env.event, spawned).await?;
            if run.finished {
                break;
            }
        }
        Ok(())
    }

    /// A run stream that ended without `RunFinished`: mark it finished, flush the full
    /// answer (including the last word held back while streaming) and warn the user.
    async fn finish_dropped_run(&self, run: &mut ActiveRun) -> Result<()> {
        run.finished = true;
        if run.is_child {
            return Ok(());
        }
        let chat_id = run.telegram_chat_id;
        if !run.answer_buf.is_empty() {
            let clean = self.redact(&run.answer_buf);
            run.editor.push(&self.inner.bot_api, &clean).await?;
            run.editor.flush_final(&self.inner.bot_api).await?;
        }
        self.send_redacted(
            chat_id,
            "⚠️ Lost the connection to the run's event stream; it may still be running.",
            None,
        )
        .await?;
        Ok(())
    }

    /// Deliver the end of a run. The run is marked finished first: the stream has ended, so
    /// even if delivering the final text fails (blocked bot, 429) it must still be cleaned up.
    async fn finish_run(&self, run: &mut ActiveRun, error: Option<String>) -> Result<()> {
        run.finished = true;
        let chat_id = run.telegram_chat_id;
        if run.is_child {
            if let Some(err) = error {
                let text = format!("❌ Subagent failed: {err}");
                self.send_redacted(chat_id, &text, None).await?;
            }
            return Ok(());
        }
        let clean = self.redact(&run.answer_buf);
        run.editor.push(&self.inner.bot_api, &clean).await?;
        run.editor.flush_final(&self.inner.bot_api).await?;
        if let Some(err) = error {
            let text = format!("❌ Run failed: {err}");
            self.send_redacted(chat_id, &text, None).await?;
        } else if run.editor.message_id().is_none() {
            self.send_redacted(chat_id, "✅ Run finished.", None)
                .await?;
        }
        Ok(())
    }

    async fn process_run_event(
        &self,
        run: &mut ActiveRun,
        event: Event,
        spawned: &mut Vec<(i64, RunId, CancellationToken)>,
    ) -> Result<()> {
        let chat_id = run.telegram_chat_id;
        match event {
            Event::MessageDelta(d) if !run.is_child => {
                run.answer_buf.push_str(&d.text);
                // Only the redaction-safe part is shown while streaming; the complete text is
                // pushed when the run finishes (see `safe_stream_prefix`).
                let clean = self.redact(safe_stream_prefix(&run.answer_buf));
                run.editor.push(&self.inner.bot_api, &clean).await?;
            }
            Event::ToolCallRequested(t) if !run.is_child => {
                run.tool_names.insert(t.call_id, t.tool_name.clone());
                // Redact BEFORE truncating, so a secret cut in half cannot slip through.
                let args = compact_json(&self.redact_value(&t.input), 160);
                let line = if args.is_empty() {
                    format!("🔧 {}", t.tool_name)
                } else {
                    format!("🔧 {} {args}", t.tool_name)
                };
                self.send_redacted(chat_id, &line, None).await?;
            }
            Event::ToolCallCompleted(t) if !run.is_child => {
                let name = run
                    .tool_names
                    .get(&t.call_id)
                    .map_or("tool", String::as_str);
                if let Some(err) = &t.error {
                    let line = format!("❌ {name}: {}", compact_one_line(&self.redact(err), 200));
                    self.send_redacted(chat_id, &line, None).await?;
                } else if let Some(preview) = &t.output_preview {
                    let line = format!(
                        "✅ {name}: {}",
                        compact_one_line(&self.redact(preview), 200)
                    );
                    self.send_redacted(chat_id, &line, None).await?;
                }
            }
            Event::ApprovalNeeded(a) => self.send_approval(chat_id, run.is_child, a).await?,
            Event::QuestionAsked(q) => {
                let rows: Vec<Vec<InlineButton>> = q
                    .options
                    .iter()
                    .enumerate()
                    .map(|(idx, opt)| {
                        vec![InlineButton::new(
                            self.redact(opt),
                            format_question_callback(q.call_id, idx),
                        )]
                    })
                    .collect();
                let markup = InlineKeyboardMarkup::from_rows(rows);
                {
                    let mut rt = self.inner.runtime.lock();
                    if rt.pending_questions.len() >= MAX_PENDING_PROMPTS {
                        rt.pending_questions.clear();
                        rt.chat_pending_question.clear();
                    }
                    rt.chat_pending_question.insert(chat_id, q.call_id);
                    rt.pending_questions.insert(
                        q.call_id,
                        PendingQuestion {
                            telegram_chat_id: chat_id,
                            options: q.options,
                        },
                    );
                }
                let text = format!("❓ {}", q.question);
                self.send_redacted(chat_id, &text, markup.as_ref()).await?;
            }
            Event::Error(e) => {
                let text = format!("⚠️ Error: {}", e.message);
                self.send_redacted(chat_id, &text, None).await?;
            }
            Event::SubagentStarted(s) => {
                spawned.push((chat_id, s.run_id, run.cancel.clone()));
            }
            Event::RunFinished(f) => self.finish_run(run, f.error).await?,
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    async fn receive(&self) -> Result<BoxStream<'static, IncomingMessage>> {
        let offset = {
            let rt = self.inner.runtime.lock();
            rt.next_offset
        };
        let updates = self
            .inner
            .bot_api
            .get_updates(offset, self.inner.config.poll_timeout_secs)
            .await?;
        // The raw Channel API has no consumer acknowledgement. Retain these
        // records in the inbox so the bridge can recover them after a restart.
        self.handle_updates(&updates)?;
        let items: Vec<_> = updates
            .into_iter()
            .filter_map(Update::into_incoming)
            .collect();
        Ok(Box::pin(stream::iter(items)))
    }

    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> Result<i64> {
        let clean = self.redact(&msg.text);
        let markup = InlineKeyboardMarkup::from_rows(msg.buttons);
        if let Some(message_id) = msg.edit_message_id {
            let edited = self
                .inner
                .bot_api
                .edit_message_text(to.0, message_id, &clean, markup.as_ref())
                .await?;
            Ok(edited.message_id)
        } else {
            let sent = self
                .inner
                .bot_api
                .send_message(to.0, &clean, markup.as_ref())
                .await?;
            Ok(sent.message_id)
        }
    }

    fn capabilities(&self) -> ChannelCaps {
        ChannelCaps::telegram_v0()
    }
}
