//! Pairing flow (one-time 6-digit codes with 10-minute expiry), unpaired-user
//! refusal rate-limiting, bot token storage in [`secret_store`], and persistent
//! channel state.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Maximum wrong `/pair` guesses before pending codes are invalidated.
pub const MAX_PAIR_ATTEMPTS: u32 = 5;

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
}

const fn default_version() -> u32 {
    1
}

#[derive(Debug, Default)]
struct StateInner {
    persisted: PersistedFile,
    pending_codes: Vec<PendingCode>,
    refused_at: HashMap<i64, Instant>,
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

fn generate_six_digit_code() -> String {
    let uuid = Uuid::now_v7();
    let bytes = uuid.as_bytes();
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write(bytes);
    let n = hasher.finish() % 1_000_000;
    format!("{n:06}")
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

        if let Some(idx) = guard
            .pending_codes
            .iter()
            .position(|c| ct_eq(&c.code, candidate))
        {
            let entry = guard.pending_codes.remove(idx);
            if entry.expires_at <= now {
                return Ok(PairAttempt::Expired);
            }
            let newly_inserted = guard.persisted.paired_users.insert(user_id);
            guard.refused_at.remove(&user_id);
            Self::save_locked(self.path.as_deref(), &guard.persisted)?;
            return Ok(if newly_inserted {
                PairAttempt::Paired
            } else {
                PairAttempt::AlreadyPaired
            });
        }

        // Wrong code: increment failure count on all live codes and burn any that exceed the cap.
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
        guard.refused_at.insert(user_id, now);
        true
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
