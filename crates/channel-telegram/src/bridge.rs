//! Telegram ↔ Daemon bridge (`TelegramConfig`, `TelegramChannel`).

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, Envelope, Event, RunId, ToolCallId};
use secret_store::{MemoryStore, Redactor, SecretRef, SecretStore};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bot_api::{
    BotApiClient, DEFAULT_EDIT_INTERVAL, DEFAULT_TELEGRAM_API_URL, InlineKeyboardMarkup,
    ThrottledEditor, Update,
};
use crate::channel_trait::{
    Channel, ChannelCaps, ChatRef, IncomingKind, IncomingMessage, InlineButton, OutgoingMessage,
};
use crate::daemon_client::{
    ApprovalDecision, DaemonClient, DeliveryKind, SendOutcome, format_approval_callback,
    format_question_callback, parse_approval_callback, parse_question_callback,
};
use crate::error::{Result, TelegramError};
use crate::pairing::{
    PairAttempt, PairingStore, TELEGRAM_BOT_TOKEN_SECRET, UNPAIRED_REFUSAL_TEXT, load_bot_token,
};

/// Default `getUpdates` long-polling timeout in seconds.
pub const DEFAULT_POLL_TIMEOUT_SECS: u64 = 25;

/// Default backoff between `getUpdates` polls on error or when active streams are open.
pub const DEFAULT_POLL_BACKOFF: Duration = Duration::from_millis(100);

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
    /// Short sleep interval between poll ticks when active runs are streaming or after errors.
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
    chat_active_run: HashMap<i64, RunId>,
    pending_questions: HashMap<ToolCallId, PendingQuestion>,
    chat_pending_question: HashMap<i64, ToolCallId>,
}

struct Inner {
    config: TelegramConfig,
    bot_api: BotApiClient,
    api_client: Arc<dyn DaemonClient>,
    pairing: PairingStore,
    redactor: Redactor,
    runtime: AsyncMutex<RuntimeState>,
    cancel: CancellationToken,
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
        let token = load_bot_token(&*config.secret_store, &config.bot_token_secret)?;
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

        let bot_api = BotApiClient::with_base_url(token, &config.api_base_url);
        let inner = Arc::new(Inner {
            config,
            bot_api,
            api_client,
            pairing,
            redactor,
            runtime: AsyncMutex::new(RuntimeState::default()),
            cancel: CancellationToken::new(),
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

    async fn run_loop(&self) {
        while !self.inner.cancel.is_cancelled() {
            let poll_res = tokio::select! {
                () = self.inner.cancel.cancelled() => break,
                r = self.poll_once() => r,
            };
            let has_active = {
                let rt = self.inner.runtime.lock().await;
                !rt.active_runs.is_empty()
            };
            if poll_res.is_err() || has_active {
                tokio::select! {
                    () = self.inner.cancel.cancelled() => break,
                    () = tokio::time::sleep(self.inner.config.poll_backoff) => {}
                }
            }
        }
    }

    /// Execute a single `getUpdates` poll pass, process all returned updates,
    /// and advance any active run event streams.
    ///
    /// # Errors
    /// Returns [`TelegramError`] on Telegram or daemon API failures.
    pub async fn poll_once(&self) -> Result<usize> {
        let (offset, timeout) = {
            let rt = self.inner.runtime.lock().await;
            let timeout = if rt.active_runs.is_empty() {
                self.inner.config.poll_timeout_secs
            } else {
                0
            };
            (rt.next_offset, timeout)
        };

        let updates = self.inner.bot_api.get_updates(offset, timeout).await?;
        let count = updates.len();

        for update in updates {
            {
                let mut rt = self.inner.runtime.lock().await;
                let next = update.update_id.saturating_add(1);
                rt.next_offset = Some(rt.next_offset.map_or(next, |cur| cur.max(next)));
            }
            self.handle_update(update).await?;
        }

        self.drain_active_runs().await?;
        Ok(count)
    }

    /// Process a single Telegram [`Update`] and advance active run streams.
    ///
    /// # Errors
    /// Returns [`TelegramError`] on Telegram or daemon API failures.
    pub async fn handle_update(&self, update: Update) -> Result<()> {
        let Some(incoming) = update.into_incoming() else {
            return Ok(());
        };
        self.handle_incoming(incoming).await?;
        self.drain_active_runs().await
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
                match msg.kind {
                    IncomingKind::Text(_) => {
                        self.send_redacted(chat_id, UNPAIRED_REFUSAL_TEXT, None)
                            .await?;
                    }
                    IncomingKind::Callback { ref query_id, .. } => {
                        self.inner
                            .bot_api
                            .answer_callback_query(query_id, Some(UNPAIRED_REFUSAL_TEXT))
                            .await?;
                    }
                }
            }
            return Ok(());
        }

        match msg.kind {
            IncomingKind::Text(text) => self.handle_paired_text(chat_id, &text).await,
            IncomingKind::Callback { query_id, data, .. } => {
                self.handle_callback(chat_id, &query_id, &data).await
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
        }
        Ok(())
    }

    async fn handle_paired_text(&self, chat_id: i64, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
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
            return self.handle_stop_command(chat_id).await;
        }
        if trimmed == "/status" || trimmed.starts_with("/status@") {
            return self.handle_status_command(chat_id).await;
        }

        // If the chat has a pending `ask_user` question, free text answers it.
        let pending_q = {
            let mut rt = self.inner.runtime.lock().await;
            if let Some(call_id) = rt.chat_pending_question.remove(&chat_id) {
                rt.pending_questions.remove(&call_id);
                Some(call_id)
            } else {
                None
            }
        };
        if let Some(call_id) = pending_q {
            self.inner
                .api_client
                .answer_question(call_id, trimmed)
                .await?;
            return Ok(());
        }

        self.forward_user_message(chat_id, trimmed).await
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

    async fn handle_stop_command(&self, chat_id: i64) -> Result<()> {
        let active_run = {
            let mut rt = self.inner.runtime.lock().await;
            let run_id = rt.chat_active_run.remove(&chat_id);
            if let Some(rid) = run_id {
                rt.active_runs.retain(|r| r.run_id != rid);
            }
            run_id
        };

        if let Some(run_id) = active_run {
            self.inner.api_client.cancel_run(run_id).await?;
            self.send_redacted(chat_id, &format!("🛑 Stopped run {run_id}."), None)
                .await?;
        } else {
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
            let rt = self.inner.runtime.lock().await;
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

    async fn forward_user_message(&self, telegram_chat_id: i64, text: &str) -> Result<()> {
        let bot_id = match self.resolve_bot_for_chat(telegram_chat_id).await {
            Ok(id) => id,
            Err(TelegramError::Daemon(msg)) if msg.contains("no bots configured") => {
                self.send_redacted(
                    telegram_chat_id,
                    "No bots configured on the daemon. Add a bot first.",
                    None,
                )
                .await?;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let daemon_chat_id = self.resolve_daemon_chat(telegram_chat_id, bot_id).await?;
        let outcome: SendOutcome = self
            .inner
            .api_client
            .send_message(daemon_chat_id, text)
            .await?;

        match outcome.delivery {
            DeliveryKind::Stopped => {
                let mut rt = self.inner.runtime.lock().await;
                if let Some(rid) = outcome
                    .run_id
                    .or_else(|| rt.chat_active_run.remove(&telegram_chat_id))
                {
                    rt.chat_active_run.remove(&telegram_chat_id);
                    rt.active_runs.retain(|r| r.run_id != rid);
                }
                drop(rt);
                self.send_redacted(telegram_chat_id, "🛑 Run stopped.", None)
                    .await?;
            }
            DeliveryKind::Queued if outcome.run_id.is_none() => {
                self.send_redacted(
                    telegram_chat_id,
                    "⏳ Message queued behind the active run.",
                    None,
                )
                .await?;
            }
            _ => {
                if let Some(run_id) = outcome.run_id {
                    let stream = self.inner.api_client.subscribe_run(run_id).await?;
                    let mut rt = self.inner.runtime.lock().await;
                    rt.chat_active_run.insert(telegram_chat_id, run_id);
                    rt.active_runs.push(ActiveRun {
                        telegram_chat_id,
                        run_id,
                        stream,
                        editor: ThrottledEditor::new(
                            telegram_chat_id,
                            self.inner.config.edit_interval,
                        ),
                        answer_buf: String::new(),
                        tool_names: HashMap::new(),
                        finished: false,
                    });
                }
            }
        }
        Ok(())
    }

    async fn handle_callback(&self, _chat_id: i64, query_id: &str, data: &str) -> Result<()> {
        if let Some((call_id, decision, scope)) = parse_approval_callback(data) {
            self.inner
                .api_client
                .decide_approval(call_id, decision, scope, None)
                .await?;
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
                let mut rt = self.inner.runtime.lock().await;
                if let Some(pq) = rt.pending_questions.remove(&call_id) {
                    rt.chat_pending_question.remove(&pq.telegram_chat_id);
                    if let Ok(idx) = payload.parse::<usize>()
                        && let Some(opt) = pq.options.get(idx)
                    {
                        opt.clone()
                    } else {
                        payload
                    }
                } else {
                    payload
                }
            };
            self.inner
                .api_client
                .answer_question(call_id, &answer)
                .await?;
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

    async fn drain_active_runs(&self) -> Result<()> {
        let mut rt = self.inner.runtime.lock().await;
        let mut pending_questions_to_insert: Vec<(ToolCallId, PendingQuestion)> = Vec::new();
        let mut finished_chats: Vec<(i64, RunId)> = Vec::new();

        for run in &mut rt.active_runs {
            loop {
                let next_env = if let Some(ready) = run.stream.next().now_or_never() {
                    ready
                } else if let Ok(item) =
                    tokio::time::timeout(Duration::from_millis(15), run.stream.next()).await
                {
                    item
                } else {
                    // Stream is still open and waiting; flush throttled text if due.
                    run.editor.flush_if_due(&self.inner.bot_api).await?;
                    break;
                };

                let Some(env) = next_env else {
                    run.finished = true;
                    finished_chats.push((run.telegram_chat_id, run.run_id));
                    break;
                };

                self.process_run_event(run, env.event, &mut pending_questions_to_insert)
                    .await?;

                if run.finished {
                    finished_chats.push((run.telegram_chat_id, run.run_id));
                    break;
                }
            }
        }

        for (call_id, pq) in pending_questions_to_insert {
            rt.chat_pending_question
                .insert(pq.telegram_chat_id, call_id);
            rt.pending_questions.insert(call_id, pq);
        }
        for (chat_id, run_id) in finished_chats {
            if rt.chat_active_run.get(&chat_id) == Some(&run_id) {
                rt.chat_active_run.remove(&chat_id);
            }
        }
        rt.active_runs.retain(|r| !r.finished);
        Ok(())
    }

    async fn process_run_event(
        &self,
        run: &mut ActiveRun,
        event: Event,
        pending_questions: &mut Vec<(ToolCallId, PendingQuestion)>,
    ) -> Result<()> {
        let chat_id = run.telegram_chat_id;
        match event {
            Event::MessageDelta(d) => {
                run.answer_buf.push_str(&d.text);
                let clean = self.redact(&run.answer_buf);
                run.editor.push(&self.inner.bot_api, &clean).await?;
            }
            Event::ToolCallRequested(t) => {
                run.tool_names.insert(t.call_id, t.tool_name.clone());
                let args = compact_json(&t.input, 160);
                let line = if args.is_empty() {
                    format!("🔧 {}", t.tool_name)
                } else {
                    format!("🔧 {} {args}", t.tool_name)
                };
                self.send_redacted(chat_id, &line, None).await?;
            }
            Event::ToolCallCompleted(t) => {
                let name = run
                    .tool_names
                    .get(&t.call_id)
                    .map_or("tool", String::as_str);
                if let Some(err) = &t.error {
                    let line = format!("❌ {name}: {}", compact_one_line(err, 200));
                    self.send_redacted(chat_id, &line, None).await?;
                } else if let Some(preview) = &t.output_preview {
                    let line = format!("✅ {name}: {}", compact_one_line(preview, 200));
                    self.send_redacted(chat_id, &line, None).await?;
                }
            }
            Event::ApprovalNeeded(a) => {
                let rows = vec![vec![
                    InlineButton::new("Approve once", format_approval_callback(a.call_id, "once")),
                    InlineButton::new("Always", format_approval_callback(a.call_id, "always")),
                    InlineButton::new("Deny", format_approval_callback(a.call_id, "deny")),
                ]];
                let markup = InlineKeyboardMarkup::from_rows(rows);
                let text = format!("⚠️ Approval needed ({})\n{}", a.tool_name, a.summary);
                self.send_redacted(chat_id, &text, markup.as_ref()).await?;
            }
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
                pending_questions.push((
                    q.call_id,
                    PendingQuestion {
                        telegram_chat_id: chat_id,
                        options: q.options,
                    },
                ));
                let text = format!("❓ {}", q.question);
                self.send_redacted(chat_id, &text, markup.as_ref()).await?;
            }
            Event::Error(e) => {
                let text = format!("⚠️ Error: {}", e.message);
                self.send_redacted(chat_id, &text, None).await?;
            }
            Event::RunFinished(f) => {
                run.editor.flush_final(&self.inner.bot_api).await?;
                if let Some(err) = f.error {
                    let text = format!("❌ Run failed: {err}");
                    self.send_redacted(chat_id, &text, None).await?;
                } else if run.editor.message_id().is_none() {
                    self.send_redacted(chat_id, "✅ Run finished.", None)
                        .await?;
                }
                run.finished = true;
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    async fn receive(&self) -> Result<BoxStream<'static, IncomingMessage>> {
        let offset = {
            let rt = self.inner.runtime.lock().await;
            rt.next_offset
        };
        let updates = self
            .inner
            .bot_api
            .get_updates(offset, self.inner.config.poll_timeout_secs)
            .await?;
        let mut items = Vec::with_capacity(updates.len());
        let mut max_next: Option<i64> = None;
        for u in updates {
            let next = u.update_id.saturating_add(1);
            max_next = Some(max_next.map_or(next, |cur| cur.max(next)));
            if let Some(inc) = u.into_incoming() {
                items.push(inc);
            }
        }
        if let Some(next) = max_next {
            let mut rt = self.inner.runtime.lock().await;
            rt.next_offset = Some(rt.next_offset.map_or(next, |cur| cur.max(next)));
        }
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
