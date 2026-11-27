//! Discord REST calls (API v10). The base address can be replaced in tests.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use secret_store::{REDACTED, SecretValue};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{DiscordError, Result};

/// The real Discord REST address.
pub const DEFAULT_API_URL: &str = "https://discord.com/api/v10";

/// Application flag: privileged "message content" intent is on.
const FLAG_MESSAGE_CONTENT: u64 = 1 << 18;
/// Application flag: same intent, for bots in fewer than 100 servers.
const FLAG_MESSAGE_CONTENT_LIMITED: u64 = 1 << 19;

/// Most 429 answers one call waits out before giving up.
const MAX_RATE_LIMIT_RETRIES: u32 = 3;
/// Longest single wait we accept from Discord's `retry_after`.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// The bot's own account, from `GET /users/@me`.
#[derive(Debug, Clone, Deserialize)]
pub struct BotUser {
    /// Account id.
    pub id: String,
    /// Account name.
    #[serde(default)]
    pub username: String,
}

/// Only ASCII digits, so an id can never change the path of a request.
#[must_use]
pub fn is_snowflake(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

fn is_interaction_token(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 512
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// REST client with a per-channel send limiter and 429 handling.
pub struct DiscordRest {
    http: reqwest::Client,
    base: String,
    token: SecretValue,
    sent: Mutex<HashMap<String, VecDeque<Instant>>>,
    burst: usize,
    window: Duration,
    cooldowns: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl fmt::Debug for DiscordRest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordRest")
            .field("base", &self.base)
            .field("token", &REDACTED)
            .finish_non_exhaustive()
    }
}

impl DiscordRest {
    /// Build a client for `base` (no trailing slash needed).
    ///
    /// # Panics
    /// Panics if the TLS backend cannot be initialised.
    #[must_use]
    pub fn new(token: SecretValue, base: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Discord HTTP client"),
            base: base.trim_end_matches('/').to_owned(),
            token,
            sent: Mutex::new(HashMap::new()),
            burst: 5,
            window: Duration::from_secs(5),
            cooldowns: Mutex::new(HashMap::new()),
        }
    }

    /// Change the own send limit: at most `burst` messages per `window` per channel.
    #[must_use]
    pub fn with_send_limit(mut self, burst: usize, window: Duration) -> Self {
        self.burst = burst.max(1);
        self.window = window;
        self
    }

    /// Wait until this channel may receive another message.
    async fn pace(&self, channel_id: &str) {
        self.pace_with(channel_id, self.burst, self.window).await;
    }

    async fn pace_with(&self, key: &str, burst: usize, window: Duration) {
        loop {
            let wait = {
                let mut sent = self.sent.lock();
                let now = Instant::now();
                let q = sent.entry(key.to_owned()).or_default();
                while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
                    q.pop_front();
                }
                if q.len() < burst {
                    q.push_back(now);
                    return;
                }
                q.front().map_or(Duration::ZERO, |t| {
                    window.saturating_sub(now.duration_since(*t))
                })
            };
            tokio::time::sleep(wait.max(Duration::from_millis(5))).await;
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        auth: bool,
    ) -> Result<Value> {
        let mut attempt = 0;
        loop {
            // Include interaction acknowledgements and retries in the global
            // budget, in addition to per-channel pacing and service cooldowns.
            self.pace_with("*", 25, Duration::from_secs(1)).await;
            let deadline = {
                let waits = self.cooldowns.lock();
                waits
                    .get(path)
                    .into_iter()
                    .chain(waits.get("*"))
                    .copied()
                    .max()
            };
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(deadline).await;
            }
            let mut rb = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .header("User-Agent", "DiscordBot (praxi-bot, 0.1)")
                .timeout(Duration::from_secs(30));
            if auth {
                rb = rb.header("Authorization", format!("Bot {}", self.token.expose()));
            }
            if let Some(b) = body {
                rb = rb.json(b);
            }
            let resp = rb
                .send()
                .await
                .map_err(|e| DiscordError::Network(e.without_url().to_string()))?;
            let status = resp.status().as_u16();
            if resp
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|v| v == "0")
                && let Some(secs) = resp
                    .headers()
                    .get("x-ratelimit-reset-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
                && secs.is_finite()
                && (0.0..=3600.0).contains(&secs)
            {
                self.cooldowns.lock().insert(
                    path.to_owned(),
                    tokio::time::Instant::now() + Duration::from_secs_f64(secs),
                );
            }
            let text = resp.text().await.unwrap_or_default();
            let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if status == 429 {
                let secs = json["retry_after"]
                    .as_f64()
                    .unwrap_or(1.0)
                    .clamp(0.0, 3600.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let wait_ms = (secs * 1000.0).ceil() as u64;
                self.cooldowns.lock().insert(
                    if json["global"] == true { "*" } else { path }.to_owned(),
                    tokio::time::Instant::now() + Duration::from_millis(wait_ms.max(1)),
                );
                if attempt >= MAX_RATE_LIMIT_RETRIES
                    || Duration::from_millis(wait_ms) > MAX_RETRY_WAIT
                {
                    return Err(DiscordError::RateLimited {
                        retry_after_ms: wait_ms,
                    });
                }
                attempt += 1;
                continue;
            }
            if status >= 300 {
                // Never log a remote response body: it can echo credentials or
                // interaction tokens, including encoded or truncated values.
                let message = match status {
                    401 => "the bot token was refused; save a fresh token",
                    403 => "the bot does not have permission for this action",
                    404 => "the Discord channel or message could not be found",
                    500..=599 => "Discord is having trouble; try again later",
                    _ => "Discord refused the request",
                }
                .to_owned();
                return Err(DiscordError::Api { status, message });
            }
            return Ok(json);
        }
    }

    /// `GET /users/@me`: who does this token belong to?
    ///
    /// # Errors
    /// Network or API errors (401 means the token is wrong).
    pub async fn get_me(&self) -> Result<BotUser> {
        let v = self
            .call(reqwest::Method::GET, "/users/@me", None, true)
            .await?;
        serde_json::from_value(v)
            .map_err(|_| DiscordError::Network("Discord returned an unreadable bot account".into()))
    }

    /// Is the privileged "message content" intent switched on for this bot?
    ///
    /// # Errors
    /// Network or API errors.
    pub async fn message_content_enabled(&self) -> Result<bool> {
        let v = self
            .call(reqwest::Method::GET, "/applications/@me", None, true)
            .await?;
        let flags = v["flags"].as_u64().unwrap_or(0);
        Ok(flags & (FLAG_MESSAGE_CONTENT | FLAG_MESSAGE_CONTENT_LIMITED) != 0)
    }

    /// Open (or find) the private channel with `user_id`; Discord answers with the same
    /// channel every time.
    ///
    /// # Errors
    /// Invalid user id, network or API errors.
    pub async fn open_dm(&self, user_id: &str) -> Result<String> {
        if !is_snowflake(user_id) {
            return Err(DiscordError::Config("invalid Discord user id".into()));
        }
        let body = json!({ "recipient_id": user_id });
        let v = self
            .call(
                reqwest::Method::POST,
                "/users/@me/channels",
                Some(&body),
                true,
            )
            .await?;
        v["id"]
            .as_str()
            .filter(|id| is_snowflake(id))
            .map(str::to_owned)
            .ok_or_else(|| DiscordError::Network("Discord did not return a channel id".into()))
    }

    /// Send a message. `components` are the button rows, if any. Mentions are
    /// always switched off for the request.
    ///
    /// # Errors
    /// Invalid channel id, network or API errors.
    pub async fn create_message(
        &self,
        channel_id: &str,
        content: &str,
        components: Option<&Value>,
    ) -> Result<String> {
        if !is_snowflake(channel_id) {
            return Err(DiscordError::Config("invalid Discord channel id".into()));
        }
        self.pace(channel_id).await;
        let body = message_body(content, components);
        let v = self
            .call(
                reqwest::Method::POST,
                &format!("/channels/{channel_id}/messages"),
                Some(&body),
                true,
            )
            .await?;
        v["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| DiscordError::Network("Discord did not return a message id".into()))
    }

    /// Edit one of our messages; no `components` removes the buttons.
    ///
    /// # Errors
    /// Invalid ids, network or API errors.
    pub async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
        components: Option<&Value>,
    ) -> Result<()> {
        if !is_snowflake(channel_id) || !is_snowflake(message_id) {
            return Err(DiscordError::Config("invalid Discord id".into()));
        }
        self.pace(channel_id).await;
        let body = message_body(content, components);
        self.call(
            reqwest::Method::PATCH,
            &format!("/channels/{channel_id}/messages/{message_id}"),
            Some(&body),
            true,
        )
        .await
        .map(|_| ())
    }

    /// Answer a button press (`type 6` = quiet acknowledge, `type 4` = a reply only
    /// the presser sees, shown when `text` is given).
    ///
    /// # Errors
    /// Invalid ids, network or API errors.
    pub async fn interaction_callback(
        &self,
        interaction_id: &str,
        token: &str,
        text: Option<&str>,
    ) -> Result<()> {
        if !is_snowflake(interaction_id) || !is_interaction_token(token) {
            return Err(DiscordError::Config("invalid interaction reference".into()));
        }
        let body = text.map_or_else(
            || json!({"type": 6}),
            |t| {
                json!({"type": 4, "data": {
                    "content": t, "flags": 64, "allowed_mentions": {"parse": []}
                }})
            },
        );
        // The callback address is authenticated by its own token, not the bot token.
        self.call(
            reqwest::Method::POST,
            &format!("/interactions/{interaction_id}/{token}/callback"),
            Some(&body),
            false,
        )
        .await
        .map(|_| ())
    }
}

fn message_body(content: &str, components: Option<&Value>) -> Value {
    json!({
        "content": content,
        "allowed_mentions": {"parse": []},
        "components": components.cloned().unwrap_or_else(|| json!([])),
    })
}
