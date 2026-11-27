//! The bridge between Discord and the daemon: who may talk, what they may
//! decide, and how a run is followed and reported.
//!
//! Security rules kept here:
//! - Only the paired owner account is ever served. Strangers get at most one
//!   polite line, in a private chat only.
//! - The owner is served in their private chat with the bot, and in server
//!   channels they allowed themselves. Never anywhere else.
//! - A button press is honoured only from the owner, in the chat the card was
//!   sent to, once, for 10 minutes, and only "approve once" or "deny", only for
//!   low-risk requests whose full text could be shown.
//! - Everything sent to Discord is redacted, then mention-escaped.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use channel_api::notice::{
    Feedback, Flavor, NoticeLang, OwnerNotice, feedback, parse_reply, parse_retry_callback, render,
    reply_error_feedback, retry_callback,
};
use channel_api::{
    Channel, ChatRef, IncomingKind, IncomingMessage, InlineButton, OutgoingMessage,
    approval::approval_row,
};
use channel_telegram::{
    ApprovalDecision, ApprovalScope, DaemonClient, DeliveryKind, NoticeBook, follow_notices,
    format_approval_callback, format_question_callback, parse_approval_callback,
    parse_question_callback,
};
use futures_util::StreamExt;
use parking_lot::Mutex;
use protocol::{Event, RunId, ToolCallId};
use secret_store::{Redactor, SecretStore, SecretValue};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{DiscordError, Result};
use crate::gateway::{DEFAULT_GATEWAY_URL, GatewayHealth, parse_chat};
use crate::messages::{Lang, Msg};
use crate::rest::{DEFAULT_API_URL, DiscordRest};
use crate::state::{DiscordState, PairOutcome};
use crate::text::{MAX_MESSAGE_CHARS, split_message};
use crate::transport::DiscordTransport;

/// Secret-store name of the Discord bot token.
pub const DISCORD_BOT_TOKEN_SECRET: &str = "discord.bot_token";

/// Longest message we pass on to the model.
const MAX_INCOMING_CHARS: usize = 8192;
/// How long an approval card stays valid.
const CARD_TTL: Duration = Duration::from_secs(10 * 60);
/// Most runs followed at once.
const MAX_ACTIVE_RUNS: usize = 16;
/// Most open cards / questions remembered.
const MAX_PENDING: usize = 256;
/// Most Discord messages one approval card may use (it must be read in full).
const MAX_CARD_PIECES: usize = 4;
/// A queued turn is looked for this long.
const QUEUED_WATCH: Duration = Duration::from_secs(30 * 60);
const SEEN_EVENTS: usize = 512;

/// Save the bot token in the secret store.
///
/// # Errors
/// Empty or malformed token, or a store error.
pub fn store_bot_token(store: &dyn SecretStore, token: &str) -> Result<()> {
    let t = token.trim();
    if t.is_empty() {
        return Err(DiscordError::Config(
            "the Discord bot token is empty".into(),
        ));
    }
    if t.len() < 20 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(DiscordError::Config(
            "that does not look like a Discord bot token (it has spaces or is too short)".into(),
        ));
    }
    store.set(DISCORD_BOT_TOKEN_SECRET, t)?;
    Ok(())
}

/// Settings of one Discord channel.
#[derive(Clone)]
pub struct DiscordConfig {
    /// Where the bot token lives.
    pub secret_store: Arc<dyn SecretStore>,
    /// Name of the token in the store.
    pub token_secret: String,
    /// REST address.
    pub api_base_url: String,
    /// Gateway address.
    pub gateway_url: String,
    /// State file; `None` keeps state in memory only.
    pub state_path: Option<PathBuf>,
    /// More secrets to hide in outgoing text.
    pub extra_redactor_secrets: Vec<String>,
    /// Language until the owner picks one.
    pub language: Lang,
    /// First wait after a lost gateway connection.
    pub reconnect_backoff: Duration,
    /// Messages per channel per window.
    pub send_burst: usize,
    /// Length of that window.
    pub send_window: Duration,
    /// How often a queued turn is looked for.
    pub queue_poll: Duration,
    /// Messages the owner may send per minute.
    pub incoming_per_minute: usize,
}

impl DiscordConfig {
    /// Defaults for production use.
    #[must_use]
    pub fn new(secret_store: Arc<dyn SecretStore>) -> Self {
        Self {
            secret_store,
            token_secret: DISCORD_BOT_TOKEN_SECRET.into(),
            api_base_url: DEFAULT_API_URL.into(),
            gateway_url: DEFAULT_GATEWAY_URL.into(),
            state_path: None,
            extra_redactor_secrets: Vec::new(),
            language: Lang::En,
            reconnect_backoff: Duration::from_secs(5),
            send_burst: 5,
            send_window: Duration::from_secs(5),
            queue_poll: Duration::from_secs(2),
            incoming_per_minute: 30,
        }
    }

    /// Test helper: a memory store holding `token`.
    #[must_use]
    pub fn with_in_memory_token(token: &str) -> Self {
        let store = secret_store::MemoryStore::new();
        let _ = store.set(DISCORD_BOT_TOKEN_SECRET, token);
        Self::new(Arc::new(store))
    }
}

impl std::fmt::Debug for DiscordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordConfig")
            .field("api_base_url", &self.api_base_url)
            .field("gateway_url", &self.gateway_url)
            .finish_non_exhaustive()
    }
}

fn valid_endpoint(address: &str, secure: &str, local: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(address) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return false;
    }
    url.scheme() == secure
        || (url.scheme() == local
            && matches!(
                url.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            ))
}

fn validate_urls(cfg: &DiscordConfig) -> Result<()> {
    if !valid_endpoint(&cfg.api_base_url, "https", "http") {
        return Err(DiscordError::Config(
            "the Discord address must use https (http only for this computer)".into(),
        ));
    }
    if !valid_endpoint(&cfg.gateway_url, "wss", "ws") {
        return Err(DiscordError::Config(
            "the Discord gateway address must use wss (ws only for this computer)".into(),
        ));
    }
    Ok(())
}

struct Card {
    owner: String,
    chat: ChatRef,
    issued: Instant,
    message_id: Option<String>,
    /// The last piece of the card as sent (already cleaned), kept to edit in the result.
    last_piece: String,
}

struct Question {
    owner: String,
    issued: Instant,
    chat: ChatRef,
    options: Vec<String>,
}

#[derive(Default)]
struct Mem {
    seen: VecDeque<String>,
    cards: HashMap<ToolCallId, Card>,
    questions: HashMap<ToolCallId, Question>,
    attached: HashSet<RunId>,
    flood: VecDeque<Instant>,
    flood_told: bool,
    tasks: Vec<JoinHandle<()>>,
    /// The owner's private chat, opened the first time a notice is pushed.
    owner_dm: Option<(String, ChatRef)>,
}

struct Inner {
    cfg: DiscordConfig,
    transport: Arc<DiscordTransport>,
    daemon: Arc<dyn DaemonClient>,
    state: DiscordState,
    cancel: CancellationToken,
    mem: Mutex<Mem>,
    runs: AtomicUsize,
    /// The owner notices pushed from this bot (what `/answer` and the retry button name).
    notices: NoticeBook,
    /// The notice follower. It outlives a re-pairing (which aborts the run tasks), so it is
    /// not kept with them.
    notice_task: Mutex<Option<JoinHandle<()>>>,
}

/// A running Discord channel.
#[derive(Clone)]
pub struct DiscordChannel {
    inner: Arc<Inner>,
    main: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for DiscordChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordChannel").finish_non_exhaustive()
    }
}

/// Split `!cmd arg` / `/cmd arg`.
fn parse_command(text: &str) -> Option<(String, &str)> {
    let t = text.trim();
    let rest = t.strip_prefix('!').or_else(|| t.strip_prefix('/'))?;
    let (cmd, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if cmd.is_empty() || !cmd.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((cmd.to_ascii_lowercase(), arg.trim()))
}

impl DiscordChannel {
    /// Build the channel without connecting.
    ///
    /// # Errors
    /// Missing token, bad addresses or unreadable state file.
    pub fn new(cfg: DiscordConfig, daemon: Arc<dyn DaemonClient>) -> Result<Self> {
        validate_urls(&cfg)?;
        let token = cfg
            .secret_store
            .get(&cfg.token_secret)?
            .filter(|t| !t.expose().trim().is_empty())
            .ok_or_else(|| DiscordError::Config("no Discord bot token is saved".into()))?;
        let mut redactor = Redactor::from_store(&*cfg.secret_store)?;
        let mut secrets = cfg.extra_redactor_secrets.clone();
        for name in cfg.secret_store.list_names()? {
            if let Some(value) = cfg.secret_store.get(&name)? {
                secrets.push(value.expose().to_owned());
            }
        }
        secrets.push(token.expose().to_owned());
        let variants: Vec<String> = secrets
            .iter()
            .flat_map(|s| secret_store::secret_variants(s))
            .collect();
        for s in &variants {
            redactor.add_secret(s);
        }
        let state = match &cfg.state_path {
            Some(p) => DiscordState::open(p)?,
            None => DiscordState::in_memory(),
        };
        let rest = Arc::new(
            DiscordRest::new(SecretValue::new(token.expose()), &cfg.api_base_url)
                .with_send_limit(cfg.send_burst, cfg.send_window),
        );
        let cancel = CancellationToken::new();
        let transport = Arc::new(
            DiscordTransport::new(
                rest,
                cfg.gateway_url.clone(),
                SecretValue::new(token.expose()),
                Arc::new(redactor),
                cfg.reconnect_backoff,
                cancel.clone(),
            )
            .with_stream_secrets(variants),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                transport,
                daemon,
                state,
                cancel,
                mem: Mutex::new(Mem::default()),
                runs: AtomicUsize::new(0),
                notices: NoticeBook::default(),
                notice_task: Mutex::new(None),
            }),
            main: Arc::new(Mutex::new(None)),
        })
    }

    /// Build and connect in a background task.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn start(cfg: DiscordConfig, daemon: Arc<dyn DaemonClient>) -> Result<Self> {
        let channel = Self::new(cfg, daemon)?;
        let worker = channel.inner.clone();
        let handle = tokio::spawn(async move { worker.run_loop().await });
        *channel.main.lock() = Some(handle);
        Ok(channel)
    }

    /// A new one-time pairing code (valid 10 minutes).
    #[must_use]
    pub fn create_pair_code(&self) -> String {
        self.inner.state.create_code()
    }

    /// Number of paired accounts (0 or 1).
    #[must_use]
    pub fn paired_count(&self) -> usize {
        self.inner.state.paired_count()
    }

    /// Is `user_id` the owner?
    #[must_use]
    pub fn is_owner(&self, user_id: &str) -> bool {
        self.inner.state.is_owner(user_id)
    }

    /// The gateway's connection state.
    #[must_use]
    pub fn health(&self) -> &Arc<GatewayHealth> {
        self.inner.transport.health()
    }

    /// The transport (tests and the connection test).
    #[must_use]
    pub fn transport(&self) -> &Arc<DiscordTransport> {
        &self.inner.transport
    }

    /// Still working (not stopped, not refused for good).
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.inner.cancel.is_cancelled()
            && !self.health().fatal.load(Ordering::SeqCst)
            && self.main.lock().as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Ask the tasks to stop.
    pub fn stop(&self) {
        self.inner.cancel.cancel();
    }

    /// Stop and wait for the tasks to end.
    pub async fn shutdown(&self) {
        self.stop();
        let main = self.main.lock().take();
        if let Some(h) = main {
            let _ = h.await;
        }
        let notices = self.inner.notice_task.lock().take();
        if let Some(h) = notices {
            let _ = h.await;
        }
        let tasks = std::mem::take(&mut self.inner.mem.lock().tasks);
        for t in tasks {
            let _ = t.await;
        }
    }
}

impl Inner {
    fn lang(&self) -> Lang {
        self.state
            .language()
            .and_then(|c| Lang::from_code(&c))
            .unwrap_or(self.cfg.language)
    }

    fn log(what: &str, _e: &dyn std::fmt::Display) {
        // Daemon errors may contain private tool arguments. Keep logs fixed.
        eprintln!("discord: {what}");
    }

    async fn say(&self, chat: &ChatRef, text: &str) -> Option<String> {
        match self
            .transport
            .send(chat.clone(), OutgoingMessage::text(text))
            .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                Self::log("could not send a message", &e);
                None
            }
        }
    }

    fn spawn(self: &Arc<Self>, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let h = tokio::spawn(fut);
        let mut mem = self.mem.lock();
        mem.tasks.retain(|t| !t.is_finished());
        mem.tasks.push(h);
    }

    /// Follow the daemon's owner notices and push each to the owner's private chat.
    fn start_notices(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let retry = self.cfg.queue_poll.max(Duration::from_millis(100));
        let cancel = self.cancel.clone();
        let daemon = Arc::clone(&self.daemon);
        let handle = tokio::spawn(async move {
            follow_notices(daemon, cancel, retry, move |notice| {
                let this = Arc::clone(&this);
                Box::pin(async move { this.push_notice(&notice).await })
            })
            .await;
        });
        *self.notice_task.lock() = Some(handle);
    }

    fn notice_lang(&self) -> NoticeLang {
        match self.lang() {
            Lang::Tr => NoticeLang::Tr,
            _ => NoticeLang::En,
        }
    }

    /// The private chat with the paired owner (`None` when nobody is paired or Discord refuses).
    async fn owner_chat(&self) -> Option<ChatRef> {
        let owner = self.state.owner()?;
        if let Some((o, chat)) = &self.mem.lock().owner_dm
            && *o == owner
        {
            return Some(chat.clone());
        }
        match self.transport.open_dm(&owner).await {
            Ok(chat) => {
                self.mem.lock().owner_dm = Some((owner, chat.clone()));
                Some(chat)
            }
            Err(e) => {
                Self::log("could not open the owner's private chat", &e);
                None
            }
        }
    }

    /// Send one owner notice to the paired owner. A run this bridge already follows reports
    /// itself in its own chat, so it is not repeated.
    pub(crate) async fn push_notice(&self, notice: &OwnerNotice) {
        let followed = notice
            .run_id
            .as_deref()
            .and_then(|r| uuid::Uuid::parse_str(r).ok())
            .is_some_and(|r| self.mem.lock().attached.contains(&RunId(r)));
        if followed {
            return;
        }
        let Some(chat) = self.owner_chat().await else {
            return;
        };
        let Some(owner) = self.state.owner() else {
            return;
        };
        let lang = self.notice_lang();
        // The transport redacts secrets and breaks mentions in everything it sends (`clean`).
        let mut msg = OutgoingMessage::text(render(notice, lang, Flavor::Discord));
        if notice.can_retry {
            msg = msg.with_buttons(vec![vec![InlineButton::new(
                feedback(lang, Feedback::TryAgainLabel),
                retry_callback(&notice.id),
            )]]);
        }
        match self.transport.send(chat, msg).await {
            // Only a notice the owner was shown can be answered or retried.
            Ok(_) => self.notices.remember(notice, &owner),
            Err(e) => Self::log("could not send a notice", &e),
        }
    }

    /// `/answer <text>`: answer the questions of the newest notice shown to the owner that still
    /// has some. Only the owner can (the caller checked), never from a forward. Only questions
    /// are answered here; an approval is never decided from a chat.
    async fn cmd_answer(&self, chat: &ChatRef, sender: &str, text: &str) {
        let lang = self.notice_lang();
        if !self.state.is_owner(sender) {
            return;
        }
        let Some(notice) = self.notices.open_question(sender) else {
            self.say(chat, &feedback(lang, Feedback::NothingOpen)).await;
            return;
        };
        let replies = match parse_reply(text, &notice) {
            Ok(r) => r,
            Err(e) => {
                self.say(chat, &feedback(lang, reply_error_feedback(e)))
                    .await;
                return;
            }
        };
        match self.daemon.answer_notice(&notice.id, &replies).await {
            Ok((answered, skipped)) => {
                self.notices.mark_answered(&notice.id, sender);
                self.say(
                    chat,
                    &feedback(lang, Feedback::Answered { answered, skipped }),
                )
                .await;
            }
            Err(e) => {
                Self::log("could not pass an answer on", &e);
                self.say(chat, &feedback(lang, Feedback::AnswerFailed))
                    .await;
            }
        }
    }

    async fn run_loop(self: Arc<Self>) {
        self.start_notices();
        let stream = match self.transport.receive().await {
            Ok(s) => s,
            Err(e) => {
                Self::log("cannot start", &e);
                return;
            }
        };
        futures_util::pin_mut!(stream);
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return,
                ev = stream.next() => {
                    let Some(ev) = ev else { return };
                    self.dispatch(ev).await;
                }
            }
        }
    }

    fn first_time(&self, event_id: &str) -> bool {
        let mut mem = self.mem.lock();
        if mem.seen.iter().any(|e| e == event_id) {
            return false;
        }
        if mem.seen.len() >= SEEN_EVENTS {
            mem.seen.pop_front();
        }
        mem.seen.push_back(event_id.to_owned());
        true
    }

    async fn dispatch(self: &Arc<Self>, ev: IncomingMessage) {
        if !self.first_time(&ev.event_id) {
            return;
        }
        match ev.kind {
            IncomingKind::Text(t) => {
                self.on_text(&ev.chat, &ev.sender.id, &t, &ev.event_id)
                    .await;
            }
            IncomingKind::Unsupported(label) => {
                if self.authorized(&ev.chat, &ev.sender.id) {
                    let text = Msg::AttachmentsNotOpened(&label).render(self.lang());
                    self.say(&ev.chat, &text).await;
                } else {
                    self.stranger(&ev.chat, &ev.sender.id).await;
                }
            }
            IncomingKind::Voice { .. } => {}
            IncomingKind::Callback {
                query_id,
                data,
                message_id,
            } => {
                let this = Arc::clone(self);
                let chat = ev.chat.clone();
                let sender = ev.sender.id.clone();
                // An authenticated gateway can still deliver a flood of clicks.
                {
                    let mut mem = self.mem.lock();
                    mem.tasks.retain(|task| !task.is_finished());
                    if mem.tasks.len() >= MAX_PENDING {
                        return;
                    }
                }
                self.spawn(async move {
                    if let Err(e) = this
                        .on_callback(&chat, &sender, &query_id, &data, message_id.as_deref())
                        .await
                    {
                        Self::log("button press failed", &e);
                    }
                });
            }
        }
    }

    /// The owner, in a private chat or in a server channel they allowed.
    fn authorized(&self, chat: &ChatRef, sender: &str) -> bool {
        let Some(parts) = parse_chat(chat) else {
            return false;
        };
        self.state.is_owner(sender)
            && (parts.guild_id.is_none() || self.state.is_allowed_channel(parts.channel_id))
    }

    /// A message from someone who may not use the bot.
    async fn stranger(&self, chat: &ChatRef, sender: &str) {
        let is_dm = parse_chat(chat).is_some_and(|p| p.guild_id.is_none());
        // Never speak up in a server; one polite line per person in a private chat.
        if is_dm && self.state.should_refuse(sender) {
            self.say(chat, &Msg::NotPaired.render(self.lang())).await;
        }
    }

    /// Slide the owner's per-minute window; `false` when too many.
    fn within_rate(&self) -> Option<bool> {
        let now = Instant::now();
        let mut mem = self.mem.lock();
        while mem
            .flood
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            mem.flood.pop_front();
        }
        if mem.flood.len() >= self.cfg.incoming_per_minute {
            let first = !mem.flood_told;
            mem.flood_told = true;
            return if first { Some(false) } else { None };
        }
        mem.flood_told = false;
        mem.flood.push_back(now);
        Some(true)
    }

    #[allow(clippy::too_many_lines)]
    async fn on_text(self: &Arc<Self>, chat: &ChatRef, sender: &str, text: &str, event_id: &str) {
        let Some(parts) = parse_chat(chat) else {
            return;
        };
        let lang = self.lang();
        let command = parse_command(text);

        if let Some((cmd, arg)) = &command
            && cmd == "pair"
        {
            self.on_pair(chat, sender, arg, parts.guild_id.is_some())
                .await;
            return;
        }
        if !self.authorized(chat, sender) {
            // The owner can allow a server channel from inside it.
            if let Some((cmd, _)) = &command
                && cmd == "allow"
                && parts.guild_id.is_some()
                && self.state.is_owner(sender)
            {
                match self.state.set_channel_allowed(parts.channel_id, true) {
                    Ok(()) => {
                        self.say(chat, &Msg::Allowed.render(lang)).await;
                    }
                    Err(e) => {
                        self.say(chat, &Msg::Failed(&e.to_string()).render(lang))
                            .await;
                    }
                }
                return;
            }
            self.stranger(chat, sender).await;
            return;
        }
        // An authenticated owner must be able to stop a run even after
        // reaching the normal message budget.
        if command.as_ref().is_some_and(|(cmd, _)| cmd == "stop") {
            if let Err(e) = self
                .forward(chat, parts.channel_id, "/stop", event_id)
                .await
            {
                self.fail(chat, &e).await;
            }
            return;
        }
        match self.within_rate() {
            Some(true) => {}
            Some(false) => {
                self.say(chat, &Msg::TooFast.render(lang)).await;
                return;
            }
            None => return,
        }

        if let Some((cmd, arg)) = command {
            match cmd.as_str() {
                "allow" => {
                    if parts.guild_id.is_none() {
                        self.say(chat, &Msg::AllowInServer.render(lang)).await;
                    } else {
                        // Already allowed (we are authorized): say so again.
                        self.say(chat, &Msg::Allowed.render(lang)).await;
                    }
                    return;
                }
                "disallow" => {
                    if parts.guild_id.is_some() {
                        match self.state.set_channel_allowed(parts.channel_id, false) {
                            Ok(()) => {
                                self.say(chat, &Msg::Disallowed.render(lang)).await;
                            }
                            Err(e) => self.fail(chat, &e).await,
                        }
                    }
                    return;
                }
                "help" | "start" => {
                    self.say(chat, &Msg::Help.render(lang)).await;
                    return;
                }
                "answer" => return self.cmd_answer(chat, sender, arg).await,
                "bots" => return self.cmd_bots(chat, parts.channel_id).await,
                "use" => return self.cmd_use(chat, parts.channel_id, arg).await,
                "status" => return self.cmd_status(chat).await,
                "lang" => {
                    match Lang::from_code(arg) {
                        Some(l) => match self.state.set_language(l.code()) {
                            Ok(()) => {
                                self.say(chat, &Msg::LangSet.render(l)).await;
                            }
                            Err(e) => self.fail(chat, &e).await,
                        },
                        None => {
                            self.say(chat, &Msg::LangUsage.render(lang)).await;
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
        if text.chars().count() > MAX_INCOMING_CHARS {
            self.say(chat, &Msg::TooLong.render(lang)).await;
            return;
        }
        if let Err(e) = self.forward(chat, parts.channel_id, text, event_id).await {
            self.fail(chat, &e).await;
        }
    }

    async fn fail(&self, chat: &ChatRef, e: &DiscordError) {
        Self::log("message not delivered", e);
        let short: String = self
            .transport
            .redact(&e.to_string())
            .chars()
            .take(200)
            .collect();
        self.say(chat, &Msg::Failed(&short).render(self.lang()))
            .await;
    }

    async fn on_pair(&self, chat: &ChatRef, sender: &str, code: &str, in_server: bool) {
        let lang = self.lang();
        if in_server {
            self.state.reject_public_code(code);
            // A code typed in public is burnt: say so, but only for something
            // that looks like a code, and only once per account per day.
            if code.len() == 6
                && code.bytes().all(|b| b.is_ascii_digit())
                && self.state.should_refuse(sender)
            {
                self.say(chat, &Msg::PairInDm.render(lang)).await;
            }
            return;
        }
        match self.state.redeem(sender, code) {
            Ok(PairOutcome::Paired) => {
                // Pending decisions and run subscriptions belong to the old
                // pairing. A replacement owner cannot inherit them.
                {
                    let mut mem = self.mem.lock();
                    mem.cards.clear();
                    mem.questions.clear();
                    mem.attached.clear();
                    // The old owner's private chat and open questions are not the new owner's.
                    mem.owner_dm = None;
                    self.notices.clear();
                    for task in mem.tasks.drain(..) {
                        task.abort();
                    }
                    self.runs.store(0, Ordering::SeqCst);
                }
                self.say(chat, &Msg::Paired.render(lang)).await;
            }
            Ok(PairOutcome::Invalid) => {
                if self.state.should_refuse(sender) {
                    self.say(chat, &Msg::PairInvalid.render(lang)).await;
                }
            }
            Ok(PairOutcome::Expired) => {
                if self.state.should_refuse(sender) {
                    self.say(chat, &Msg::PairExpired.render(lang)).await;
                }
            }
            Ok(PairOutcome::RateLimited) => {}
            Err(e) => {
                self.fail(chat, &e).await;
            }
        }
    }

    async fn cmd_bots(&self, chat: &ChatRef, channel_id: &str) {
        let lang = self.lang();
        match self.daemon.list_bots().await {
            Ok(bots) if bots.is_empty() => {
                self.say(chat, &Msg::NoBots.render(lang)).await;
            }
            Ok(bots) => {
                let current = self.state.bot_for(channel_id);
                let mut out = Msg::BotsHeading.render(lang);
                for b in bots.iter().take(25) {
                    let mark = if Some(b.id) == current { " ✓" } else { "" };
                    let _ = write!(
                        out,
                        "
• {}{mark}",
                        b.name
                    );
                }
                self.say(chat, &out).await;
            }
            Err(e) => self.fail(chat, &e.into()).await,
        }
    }

    async fn cmd_use(&self, chat: &ChatRef, channel_id: &str, name: &str) {
        let lang = self.lang();
        if name.is_empty() {
            self.say(chat, &Msg::UseUsage.render(lang)).await;
            return;
        }
        let bots = match self.daemon.list_bots().await {
            Ok(b) => b,
            Err(e) => return self.fail(chat, &e.into()).await,
        };
        let wanted = name.to_lowercase();
        let found = bots
            .iter()
            .find(|b| b.name.to_lowercase() == wanted)
            .or_else(|| {
                bots.iter()
                    .find(|b| b.name.to_lowercase().contains(&wanted))
            });
        match found {
            Some(b) => match self.state.set_bot_for(channel_id, b.id) {
                Ok(()) => {
                    self.say(chat, &Msg::UseOk(&b.name).render(lang)).await;
                }
                Err(e) => self.fail(chat, &e).await,
            },
            None => {
                self.say(chat, &Msg::UseUnknown.render(lang)).await;
            }
        }
    }

    async fn cmd_status(&self, chat: &ChatRef) {
        match self.daemon.status().await {
            Ok(s) => {
                let running = self.runs.load(Ordering::SeqCst);
                self.say(
                    chat,
                    &Msg::Status {
                        running,
                        pending: s.pending_approvals,
                    }
                    .render(self.lang()),
                )
                .await;
            }
            Err(e) => self.fail(chat, &e.into()).await,
        }
    }

    async fn resolve_chat(&self, channel_id: &str) -> Result<Option<protocol::ChatId>> {
        let bot = match self.state.bot_for(channel_id) {
            Some(b) => b,
            None => match self.daemon.list_bots().await?.first() {
                Some(b) => b.id,
                None => return Ok(None),
            },
        };
        if let Some(c) = self.state.thread_for(channel_id, bot) {
            return Ok(Some(c));
        }
        let created = self.daemon.create_chat(bot, Some("Discord")).await?;
        self.state.set_thread_for(channel_id, bot, created.id)?;
        Ok(Some(created.id))
    }

    async fn forward(
        self: &Arc<Self>,
        chat: &ChatRef,
        channel_id: &str,
        text: &str,
        event_id: &str,
    ) -> Result<()> {
        let lang = self.lang();
        let Some(daemon_chat) = self.resolve_chat(channel_id).await? else {
            self.say(chat, &Msg::NoBots.render(lang)).await;
            return Ok(());
        };
        let sent_at = now_ms();
        let outcome = self
            .daemon
            .send_message_idempotent(daemon_chat, text, &format!("dc:{event_id}"))
            .await?;
        match outcome.delivery {
            DeliveryKind::Stopped => {
                self.say(chat, &Msg::RunStopped.render(lang)).await;
            }
            DeliveryKind::Queued if outcome.run_id.is_none() => {
                self.say(chat, &Msg::Queued.render(lang)).await;
                let this = Arc::clone(self);
                let chat = chat.clone();
                self.spawn(async move { this.watch_queued(chat, daemon_chat, sent_at).await });
            }
            _ => {
                if let Some(run) = outcome.run_id {
                    self.follow(chat.clone(), run, false);
                }
            }
        }
        Ok(())
    }

    /// A queued message gets its own run later; find it and follow it.
    async fn watch_queued(
        self: Arc<Self>,
        chat: ChatRef,
        daemon_chat: protocol::ChatId,
        since_ms: i64,
    ) {
        let started = Instant::now();
        while started.elapsed() < QUEUED_WATCH {
            tokio::select! {
                () = self.cancel.cancelled() => return,
                () = tokio::time::sleep(self.cfg.queue_poll) => {}
            }
            let exclude: Vec<RunId> = self.mem.lock().attached.iter().copied().collect();
            if let Ok(Some(run)) = self
                .daemon
                .find_chat_run(daemon_chat, &exclude, since_ms)
                .await
            {
                self.follow(chat, run, false);
                return;
            }
        }
    }

    /// Follow one run in the background (a run is never followed twice).
    fn follow(self: &Arc<Self>, chat: ChatRef, run: RunId, is_child: bool) {
        {
            let mut mem = self.mem.lock();
            if mem.attached.contains(&run) {
                return;
            }
            if self.runs.load(Ordering::SeqCst) >= MAX_ACTIVE_RUNS {
                drop(mem);
                let this = Arc::clone(self);
                self.spawn(async move {
                    this.say(&chat, &Msg::TooManyRuns.render(this.lang())).await;
                });
                return;
            }
            if mem.attached.len() > 4096 {
                mem.attached.clear();
            }
            mem.attached.insert(run);
        }
        self.runs.fetch_add(1, Ordering::SeqCst);
        let this = Arc::clone(self);
        self.spawn(async move {
            this.follow_run(&chat, run, is_child).await;
            this.runs.fetch_sub(1, Ordering::SeqCst);
        });
    }

    async fn publish_answer(&self, chat: &ChatRef, text: &str, sent: &mut Vec<(String, String)>) {
        if text.is_empty() {
            return;
        }
        let clean = self.transport.clean(text);
        for (i, piece) in split_message(&clean, MAX_MESSAGE_CHARS)
            .into_iter()
            .enumerate()
        {
            if let Some((id, previous)) = sent.get_mut(i) {
                if *previous == piece {
                    continue;
                }
                match self
                    .transport
                    .send(chat.clone(), OutgoingMessage::edit(id.clone(), &piece))
                    .await
                {
                    Ok(_) => *previous = piece,
                    Err(e) => {
                        Self::log("could not update a reply", &e);
                        return;
                    }
                }
            } else {
                match self
                    .transport
                    .send(chat.clone(), OutgoingMessage::text(&piece))
                    .await
                {
                    Ok(id) => sent.push((id, piece)),
                    Err(e) => {
                        Self::log("could not send a reply", &e);
                        return;
                    }
                }
            }
        }
    }

    async fn follow_run(self: &Arc<Self>, chat: &ChatRef, run: RunId, is_child: bool) {
        let Some(owner) = self.state.owner() else {
            return;
        };
        let lang = self.lang();
        let mut stream = match self.daemon.subscribe_run(run).await {
            Ok(s) => s,
            Err(e) => {
                if !is_child {
                    self.fail(chat, &e.into()).await;
                }
                return;
            }
        };
        let mut answer = String::new();
        let mut sent = Vec::new();
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let env = tokio::select! {
                () = self.cancel.cancelled() => return,
                e = stream.next() => e,
                _ = tick.tick(), if !is_child => {
                    if !self.authorized(chat, &owner) { return; }
                    self.publish_answer(chat, self.transport.stream_prefix(&answer), &mut sent).await;
                    continue;
                }
            };
            if !self.authorized(chat, &owner) {
                return;
            }
            let Some(env) = env else {
                if !is_child {
                    if !answer.is_empty() {
                        self.publish_answer(chat, &answer, &mut sent).await;
                    }
                    self.say(chat, &Msg::StreamLost.render(lang)).await;
                }
                return;
            };
            match env.event {
                Event::MessageDelta(d) if !is_child => answer.push_str(&d.text),
                Event::ApprovalNeeded(a) => self.send_approval(chat, a).await,
                Event::QuestionAsked(q) => self.send_question(chat, q).await,
                Event::Error(e) if !is_child => {
                    self.say(chat, &Msg::RunError(&e.message).render(lang))
                        .await;
                }
                Event::SubagentStarted(s) => self.follow(chat.clone(), s.run_id, true),
                Event::RunFinished(f) => {
                    if is_child {
                        return;
                    }
                    if !answer.is_empty() {
                        self.publish_answer(chat, &answer, &mut sent).await;
                    }
                    if let Some(err) = f.error {
                        self.say(chat, &Msg::RunFailed(&err).render(lang)).await;
                    } else if answer.is_empty() {
                        self.say(chat, &Msg::RunFinished.render(lang)).await;
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    async fn send_question(&self, chat: &ChatRef, q: protocol::event::QuestionAsked) {
        let rows: Vec<Vec<InlineButton>> = q
            .options
            .iter()
            .enumerate()
            .take(5)
            .map(|(i, o)| {
                vec![InlineButton::new(
                    o.clone(),
                    format_question_callback(q.call_id, i),
                )]
            })
            .collect();
        {
            let mut mem = self.mem.lock();
            if mem.questions.len() >= MAX_PENDING {
                mem.questions.clear();
            }
            mem.questions.insert(
                q.call_id,
                Question {
                    owner: self.state.owner().unwrap_or_default(),
                    issued: Instant::now(),
                    chat: chat.clone(),
                    options: q.options.iter().take(5).cloned().collect(),
                },
            );
        }
        let mut msg = OutgoingMessage::text(format!("❓ {}", q.question));
        if !rows.is_empty() {
            msg = msg.with_buttons(rows);
        }
        if let Err(e) = self.transport.send(chat.clone(), msg).await {
            self.mem.lock().questions.remove(&q.call_id);
            Self::log("could not send a question", &e);
        }
    }

    /// Show an approval. Buttons exist only for low-risk requests whose whole
    /// text could be shown here; everything else is decided in the app.
    #[allow(clippy::too_many_lines)]
    async fn send_approval(&self, chat: &ChatRef, a: protocol::event::ApprovalNeeded) {
        let lang = self.lang();
        let tool = a.tool_name.as_str();
        let low_risk = a
            .details
            .as_ref()
            .and_then(|d| d.get("risk"))
            .and_then(serde_json::Value::as_str)
            == Some("low");
        if !low_risk {
            self.say(chat, &Msg::ApprovalDesktopOnly(tool).render(lang))
                .await;
            return;
        }
        let heading = Msg::ApprovalHeading(tool).render(lang);
        let (body, offer) = match &a.details {
            None => (format!("{heading}\n{}", a.summary), true),
            Some(d)
                if ["kind", "egress", "computer_target"]
                    .iter()
                    .all(|k| d.get(k).is_none()) =>
            {
                (format!("{heading}\n{}", a.summary), true)
            }
            Some(d) => match protocol::approval_text::describe_details(d) {
                Some(full) => {
                    let shown = format!("{heading}\n{full}");
                    // If hiding secrets would change the text, the owner could not
                    // review what is really asked: leave it to the app.
                    if self.transport.redact(&shown) == shown {
                        (shown, true)
                    } else {
                        (Msg::ApprovalMasked(tool).render(lang), false)
                    }
                }
                None => (Msg::ApprovalDetailsUnavailable(tool).render(lang), false),
            },
        };
        // This applies to summary-only cards too: a redacted request cannot
        // be reviewed in full, so it must be decided in the desktop app.
        if offer && self.transport.redact(&body) != body {
            self.say(chat, &Msg::ApprovalMasked(tool).render(lang))
                .await;
            return;
        }
        let pieces = split_message(&self.transport.clean(&body), MAX_MESSAGE_CHARS);
        if offer && pieces.len() > MAX_CARD_PIECES {
            self.say(chat, &Msg::ApprovalTooLarge(tool).render(lang))
                .await;
            return;
        }
        if !offer {
            self.say(chat, &body).await;
            return;
        }
        // `has_details = true` tells the shared policy "no standing grant": this
        // channel offers "approve once" and "deny" only, for every request.
        let row = approval_row(
            true,
            false,
            InlineButton::new(
                Msg::ButtonApprove.render(lang),
                format_approval_callback(a.call_id, "once"),
            ),
            None,
            InlineButton::new("", format_approval_callback(a.call_id, "always")),
            InlineButton::new(
                Msg::ButtonDeny.render(lang),
                format_approval_callback(a.call_id, "deny"),
            ),
        );
        {
            let mut mem = self.mem.lock();
            if mem.cards.len() >= MAX_PENDING {
                mem.cards.clear();
            }
            mem.cards.insert(
                a.call_id,
                Card {
                    owner: self.state.owner().unwrap_or_default(),
                    chat: chat.clone(),
                    issued: Instant::now(),
                    message_id: None,
                    last_piece: pieces.last().cloned().unwrap_or_default(),
                },
            );
        }
        let sent = self
            .transport
            .send(
                chat.clone(),
                OutgoingMessage::text(body).with_buttons(vec![row]),
            )
            .await;
        match sent {
            Ok(id) => {
                if let Some(card) = self.mem.lock().cards.get_mut(&a.call_id) {
                    card.message_id = Some(id);
                }
            }
            Err(e) => {
                // Never leave a button for something the owner could not read in full.
                self.mem.lock().cards.remove(&a.call_id);
                Self::log("approval card not delivered", &e);
                self.say(chat, &Msg::ApprovalIncomplete.render(lang)).await;
            }
        }
    }

    async fn answer(&self, query_id: &str, text: &str) {
        if let Err(e) = self.transport.answer_callback(query_id, Some(text)).await {
            Self::log("could not answer a button press", &e);
        }
    }

    async fn on_callback(
        &self,
        chat: &ChatRef,
        sender: &str,
        query_id: &str,
        data: &str,
        message_id: Option<&str>,
    ) -> Result<()> {
        let lang = self.lang();
        if !self.authorized(chat, sender) {
            // A stranger clicking a public card must not spend our REST budget.
            return Ok(());
        }
        if let Some((call_id, decision, scope)) = parse_approval_callback(data) {
            // Only "approve once" and "deny" exist here.
            if scope != ApprovalScope::Once {
                self.answer(query_id, &Msg::UnknownOrExpired.render(lang))
                    .await;
                return Ok(());
            }
            let card = {
                let mut mem = self.mem.lock();
                match mem.cards.remove(&call_id) {
                    Some(c)
                        if c.chat == *chat
                            && c.owner == sender
                            && c.message_id.as_deref() == message_id
                            && message_id.is_some()
                            && c.issued.elapsed() < CARD_TTL =>
                    {
                        Some(c)
                    }
                    // Another chat must not burn the owner's card.
                    Some(c) if c.chat != *chat || c.message_id.as_deref() != message_id => {
                        mem.cards.insert(call_id, c);
                        None
                    }
                    _ => None,
                }
            };
            let Some(card) = card else {
                self.answer(query_id, &Msg::UnknownOrExpired.render(lang))
                    .await;
                return Ok(());
            };
            if let Err(e) = self
                .daemon
                .decide_approval(call_id, decision, ApprovalScope::Once, None)
                .await
            {
                // Let the owner press again.
                self.mem.lock().cards.insert(call_id, card);
                self.answer(query_id, &Msg::Failed(&e.to_string()).render(lang))
                    .await;
                return Ok(());
            }
            let verdict = match decision {
                ApprovalDecision::Approve => Msg::Approved,
                ApprovalDecision::Deny => Msg::Denied,
            }
            .render(lang);
            self.answer(query_id, &verdict).await;
            if let Some(id) = card.message_id {
                let edited = format!("{}\n\n{verdict}", card.last_piece);
                if let Err(e) = self
                    .transport
                    .send(chat.clone(), OutgoingMessage::edit(id, edited))
                    .await
                {
                    Self::log("could not update the approval card", &e);
                }
            }
            return Ok(());
        }
        if let Some(notice_id) = parse_retry_callback(data) {
            let nl = self.notice_lang();
            let ok = self.state.is_owner(sender)
                && self.notices.knows(notice_id, sender)
                && self.daemon.retry_notice(notice_id).await.is_ok();
            let f = if ok {
                Feedback::Retried
            } else {
                Feedback::RetryFailed
            };
            self.answer(query_id, &feedback(nl, f)).await;
            return Ok(());
        }
        if let Some((call_id, payload)) = parse_question_callback(data) {
            let answer = {
                let mem = self.mem.lock();
                mem.questions.get(&call_id).and_then(|q| {
                    (q.chat == *chat && q.owner == sender && q.issued.elapsed() < CARD_TTL)
                        .then(|| payload.parse::<usize>().ok().and_then(|i| q.options.get(i)))
                        .flatten()
                        .cloned()
                })
            };
            let Some(answer) = answer else {
                self.answer(query_id, &Msg::UnknownOrExpired.render(lang))
                    .await;
                return Ok(());
            };
            self.daemon.answer_question(call_id, &answer).await?;
            self.mem.lock().questions.remove(&call_id);
            self.answer(query_id, &Msg::Answered.render(lang)).await;
            return Ok(());
        }
        self.transport.answer_callback(query_id, None).await?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
