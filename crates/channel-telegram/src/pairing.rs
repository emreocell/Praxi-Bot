//! Pairing flow (one-time 6-digit codes with 10-minute expiry), unpaired-user
//! refusal rate-limiting, bot token storage in [`secret_store`], and persistent
//! channel state.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use protocol::{BotId, ChatId};
use secret_store::{SecretRef, SecretStore, SecretValue, resolve};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, TelegramError};

/// Default [`SecretStore`] key name under which the Telegram bot token is stored.
pub const TELEGRAM_BOT_TOKEN_SECRET: &str = "telegram.bot_token";

/// Validity window of a one-time 6-digit pairing code (10 minutes).
pub const PAIR_CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// Cooldown window before an unpaired user who already received the polite
/// refusal message could ever receive another one.
pub const REFUSAL_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// Maximum wrong `/pair` guesses (from anyone) before pending codes are invalidated.
/// This is the global brute-force cap: a 6-digit code survives at most this many guesses.
pub const MAX_PAIR_ATTEMPTS: u32 = 5;

/// Maximum wrong `/pair` guesses per Telegram user before that user is locked out.
pub const MAX_PAIR_ATTEMPTS_PER_USER: u32 = 5;

/// How long a user stays locked out after exceeding [`MAX_PAIR_ATTEMPTS_PER_USER`].
pub const PAIR_LOCKOUT: Duration = Duration::from_secs(60 * 60);

/// Global cap of polite refusal messages per minute (flood protection).
pub const MAX_REFUSALS_PER_MINUTE: usize = 30;

/// Upper bound for per-user bookkeeping maps (memory-exhaustion protection).
const MAX_TRACKED_USERS: usize = 10_000;

/// A persisted polling offset older than this is ignored (Telegram keeps updates 24 h).
const OFFSET_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Polite refusal message sent once to an unpaired user.
pub const UNPAIRED_REFUSAL_TEXT: &str = "This bot is private and your Telegram account is not paired. Run the pairing command on your computer and send `/pair <6-digit-code>` here.";

/// Store the Telegram bot token in `store` under [`TELEGRAM_BOT_TOKEN_SECRET`].
///
/// # Errors
/// Returns [`TelegramError::Config`] if `token` is empty, or [`TelegramError::Secret`] on store errors.
pub fn store_bot_token(store: &dyn SecretStore, token: &str) -> Result<SecretRef> {
    store_bot_token_as(store, TELEGRAM_BOT_TOKEN_SECRET, token)
}

/// Store the Telegram bot token in `store` under a custom secret name.
///
/// # Errors
/// Returns [`TelegramError::Config`] if `token` is empty, or [`TelegramError::Secret`] on store errors.
pub fn store_bot_token_as(store: &dyn SecretStore, name: &str, token: &str) -> Result<SecretRef> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(TelegramError::Config(
            "telegram bot token must not be empty".into(),
        ));
    }
    let secret_ref = SecretRef::new(name)?;
    store.set(secret_ref.name(), trimmed)?;
    Ok(secret_ref)
}

/// Resolve the Telegram bot token from `store` using `secret_ref`.
///
/// # Errors
/// Returns [`TelegramError::Secret`] if the secret is missing or the backend fails.
pub fn load_bot_token(store: &dyn SecretStore, secret_ref: &SecretRef) -> Result<SecretValue> {
    Ok(resolve(store, secret_ref)?)
}

/// Outcome of attempting to redeem a `/pair <code>` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairAttempt {
    /// The code was valid; the user id is now paired and stored.
    Paired,
    /// The user was already paired; the code (if valid) was consumed.
    AlreadyPaired,
    /// The code expired before redemption.
    Expired,
    /// The code did not match any active pairing invite.
    InvalidCode,
    /// The user made too many wrong guesses and is locked out (no reply should be sent).
    RateLimited,
}

#[derive(Debug, Clone)]
struct PendingCode {
    code: String,
    expires_at: Instant,
    failed_attempts: u32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PersistedFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    paired_users: BTreeSet<i64>,
    #[serde(default)]
    chat_bots: BTreeMap<i64, BotId>,
    #[serde(default)]
    chat_threads: BTreeMap<String, ChatId>,
    /// Next `getUpdates` offset, so a restart never replays confirmed updates.
    #[serde(default)]
    next_offset: Option<i64>,
    /// Bot (token prefix) the offset belongs to.
    #[serde(default)]
    offset_bot: Option<String>,
    /// Unix seconds when the offset was saved.
    #[serde(default)]
    offset_saved_at: u64,
}

const fn default_version() -> u32 {
    1
}

#[derive(Debug, Default)]
struct StateInner {
    persisted: PersistedFile,
    pending_codes: Vec<PendingCode>,
    refused_at: HashMap<i64, Instant>,
    refusal_times: VecDeque<Instant>,
    /// Per-user wrong-guess counter and optional lockout deadline.
    pair_failures: HashMap<i64, (u32, Option<Instant>)>,
}

/// Manages paired Telegram user IDs, one-time 6-digit pairing codes, refusal
/// rate-limiting, and per-chat bot/thread mappings.
#[derive(Debug)]
pub struct PairingStore {
    path: Option<PathBuf>,
    inner: Mutex<StateInner>,
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Uniform 6-digit code from the OS CSPRNG (via a v4 UUID), using rejection sampling.
fn generate_six_digit_code() -> String {
    const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
    loop {
        let uuid = Uuid::new_v4();
        let b = uuid.as_bytes();
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        if n < LIMIT {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn thread_key(telegram_chat_id: i64, bot_id: BotId) -> String {
    format!("{telegram_chat_id}:{bot_id}")
}

impl Default for PairingStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl PairingStore {
    /// Create an in-memory store (not backed by a file on disk).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            inner: Mutex::new(StateInner {
                persisted: PersistedFile {
                    version: 1,
                    ..PersistedFile::default()
                },
                ..StateInner::default()
            }),
        }
    }

    /// Open (or create) a store persisted to `path` as JSON.
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if an existing file cannot be read or parsed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let persisted = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<PersistedFile>(&text)
                .map_err(|e| TelegramError::Storage(e.to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PersistedFile {
                version: 1,
                ..PersistedFile::default()
            },
            Err(e) => return Err(TelegramError::Storage(e.to_string())),
        };
        Ok(Self {
            path: Some(path),
            inner: Mutex::new(StateInner {
                persisted,
                ..StateInner::default()
            }),
        })
    }

    fn save_locked(path: Option<&Path>, data: &PersistedFile) -> Result<()> {
        let Some(path) = path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TelegramError::Storage(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| TelegramError::Storage(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| TelegramError::Storage(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| TelegramError::Storage(e.to_string()))
    }

    /// Generate a new one-time 6-digit pairing code valid for [`PAIR_CODE_TTL`] (10 minutes).
    #[must_use]
    pub fn create_code(&self) -> String {
        self.create_code_with_ttl(PAIR_CODE_TTL)
    }

    /// Generate a new one-time 6-digit pairing code with a custom validity duration.
    #[must_use]
    pub fn create_code_with_ttl(&self, ttl: Duration) -> String {
        let code = generate_six_digit_code();
        let now = Instant::now();
        let mut guard = self.inner.lock();
        // Keep recently expired codes briefly so we can report `Expired` vs `InvalidCode`.
        guard
            .pending_codes
            .retain(|c| c.expires_at + Duration::from_secs(600) > now);
        guard.pending_codes.push(PendingCode {
            code: code.clone(),
            expires_at: now + ttl,
            failed_attempts: 0,
        });
        code
    }

    /// Attempt to pair `user_id` using `code`.
    ///
    /// On success, the one-time code is consumed and `user_id` is added to the
    /// paired set (and persisted if backed by a file).
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if persisting the newly paired user fails.
    pub fn redeem_code(&self, user_id: i64, code: &str) -> Result<PairAttempt> {
        let candidate = code.trim();
        let now = Instant::now();
        let mut guard = self.inner.lock();

        // Per-user brute-force lockout.
        if let Some((_, Some(until))) = guard.pair_failures.get(&user_id) {
            if *until > now {
                return Ok(PairAttempt::RateLimited);
            }
            guard.pair_failures.remove(&user_id);
        }

        // Compare against every pending code (no early exit on the first byte mismatch).
        let mut matched: Option<usize> = None;
        for (idx, pending) in guard.pending_codes.iter().enumerate() {
            if ct_eq(&pending.code, candidate) && matched.is_none() {
                matched = Some(idx);
            }
        }
        if let Some(idx) = matched {
            let entry = guard.pending_codes.remove(idx);
            if entry.expires_at <= now {
                return Ok(PairAttempt::Expired);
            }
            guard.pair_failures.remove(&user_id);
            let newly_inserted = guard.persisted.paired_users.insert(user_id);
            guard.refused_at.remove(&user_id);
            Self::save_locked(self.path.as_deref(), &guard.persisted)?;
            return Ok(if newly_inserted {
                PairAttempt::Paired
            } else {
                PairAttempt::AlreadyPaired
            });
        }

        // Wrong code: count it against the user and against all live codes.
        if guard.pair_failures.len() >= MAX_TRACKED_USERS {
            guard
                .pair_failures
                .retain(|_, (_, until)| until.is_some_and(|u| u > now));
        }
        let entry = guard.pair_failures.entry(user_id).or_insert((0, None));
        entry.0 += 1;
        if entry.0 >= MAX_PAIR_ATTEMPTS_PER_USER {
            entry.1 = Some(now + PAIR_LOCKOUT);
        }
        for pending in &mut guard.pending_codes {
            if pending.expires_at > now {
                pending.failed_attempts += 1;
            }
        }
        guard
            .pending_codes
            .retain(|c| c.failed_attempts < MAX_PAIR_ATTEMPTS);

        Ok(PairAttempt::InvalidCode)
    }

    /// Directly mark `user_id` as paired (e.g. from initial config).
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if persistence fails.
    pub fn add_paired_user(&self, user_id: i64) -> Result<()> {
        let mut guard = self.inner.lock();
        if guard.persisted.paired_users.insert(user_id) {
            guard.refused_at.remove(&user_id);
            Self::save_locked(self.path.as_deref(), &guard.persisted)?;
        }
        Ok(())
    }

    /// Whether `user_id` is currently paired.
    #[must_use]
    pub fn is_paired(&self, user_id: i64) -> bool {
        self.inner.lock().persisted.paired_users.contains(&user_id)
    }

    /// Sorted list of all paired Telegram user IDs.
    #[must_use]
    pub fn paired_users(&self) -> Vec<i64> {
        self.inner
            .lock()
            .persisted
            .paired_users
            .iter()
            .copied()
            .collect()
    }

    /// Check whether an unpaired `user_id` should receive the single polite refusal
    /// message now, recording the refusal so subsequent messages are ignored.
    #[must_use]
    pub fn should_send_refusal(&self, user_id: i64) -> bool {
        let now = Instant::now();
        let mut guard = self.inner.lock();
        if let Some(&last) = guard.refused_at.get(&user_id)
            && now.duration_since(last) < REFUSAL_COOLDOWN
        {
            return false;
        }
        // Global flood cap: a swarm of distinct accounts cannot make us spam Telegram.
        while let Some(&front) = guard.refusal_times.front() {
            if now.duration_since(front) >= Duration::from_secs(60) {
                guard.refusal_times.pop_front();
            } else {
                break;
            }
        }
        if guard.refusal_times.len() >= MAX_REFUSALS_PER_MINUTE {
            return false;
        }
        if guard.refused_at.len() >= MAX_TRACKED_USERS {
            guard
                .refused_at
                .retain(|_, t| now.duration_since(*t) < REFUSAL_COOLDOWN);
            if guard.refused_at.len() >= MAX_TRACKED_USERS {
                guard.refused_at.clear();
            }
        }
        guard.refusal_times.push_back(now);
        guard.refused_at.insert(user_id, now);
        true
    }

    /// Polling offset persisted for `bot_key`, if it is recent enough to be trusted.
    #[must_use]
    pub fn saved_offset(&self, bot_key: &str) -> Option<i64> {
        let guard = self.inner.lock();
        let p = &guard.persisted;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        if p.offset_bot.as_deref() != Some(bot_key)
            || now.saturating_sub(p.offset_saved_at) > OFFSET_MAX_AGE.as_secs()
        {
            return None;
        }
        p.next_offset
    }

    /// Persist the next `getUpdates` offset for `bot_key` (best effort for in-memory stores).
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if persistence fails.
    pub fn save_offset(&self, bot_key: &str, offset: i64) -> Result<()> {
        let mut guard = self.inner.lock();
        if guard.persisted.next_offset == Some(offset)
            && guard.persisted.offset_bot.as_deref() == Some(bot_key)
        {
            return Ok(());
        }
        guard.persisted.next_offset = Some(offset);
        guard.persisted.offset_bot = Some(bot_key.to_owned());
        guard.persisted.offset_saved_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self::save_locked(self.path.as_deref(), &guard.persisted)
    }

    /// Get the bot currently mapped to `telegram_chat_id`.
    #[must_use]
    pub fn bot_for_chat(&self, telegram_chat_id: i64) -> Option<BotId> {
        self.inner
            .lock()
            .persisted
            .chat_bots
            .get(&telegram_chat_id)
            .copied()
    }

    /// Map `telegram_chat_id` to `bot_id` (used by `/use <bot>`).
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if persistence fails.
    pub fn set_bot_for_chat(&self, telegram_chat_id: i64, bot_id: BotId) -> Result<()> {
        let mut guard = self.inner.lock();
        guard.persisted.chat_bots.insert(telegram_chat_id, bot_id);
        Self::save_locked(self.path.as_deref(), &guard.persisted)
    }

    /// Get the daemon [`ChatId`] previously created for `(telegram_chat_id, bot_id)`.
    #[must_use]
    pub fn thread_for(&self, telegram_chat_id: i64, bot_id: BotId) -> Option<ChatId> {
        let key = thread_key(telegram_chat_id, bot_id);
        self.inner.lock().persisted.chat_threads.get(&key).copied()
    }

    /// Record the daemon [`ChatId`] for `(telegram_chat_id, bot_id)`.
    ///
    /// # Errors
    /// Returns [`TelegramError::Storage`] if persistence fails.
    pub fn set_thread_for(
        &self,
        telegram_chat_id: i64,
        bot_id: BotId,
        chat_id: ChatId,
    ) -> Result<()> {
        let key = thread_key(telegram_chat_id, bot_id);
        let mut guard = self.inner.lock();
        guard.persisted.chat_threads.insert(key, chat_id);
        Self::save_locked(self.path.as_deref(), &guard.persisted)
    }
}
