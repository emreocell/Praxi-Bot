//! What the Discord adapter remembers: the one paired owner account, the server
//! channels the owner allowed, and which bot / daemon chat each Discord channel
//! talks to. Plain JSON, written atomically; never holds the bot token.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use channel_api::control::OwnerId;
use channel_api::pairing::{PAIR_CODE_TTL, PairingInvites, PairingMatch};
use parking_lot::Mutex;
use protocol::{BotId, ChatId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{DiscordError, Result};

/// Cooldown before the same stranger gets another polite refusal.
const REFUSAL_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Refusals per minute over all strangers together (flood protection).
const MAX_REFUSALS_PER_MINUTE: usize = 10;
const MAX_TRACKED: usize = 10_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StateFile {
    /// The only Discord account that may talk to the bot.
    #[serde(default)]
    owner_id: Option<String>,
    /// Interface language (`en`, `tr`, `de`).
    #[serde(default)]
    language: Option<String>,
    /// Server channels where the owner said the bot may answer.
    #[serde(default)]
    allowed_channels: BTreeSet<String>,
    /// Discord channel id -> bot chosen there.
    #[serde(default)]
    bots: BTreeMap<String, BotId>,
    /// `<channel id>:<bot id>` -> daemon chat.
    #[serde(default)]
    threads: BTreeMap<String, ChatId>,
}

/// Result of trying a pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairOutcome {
    /// This account is now the owner.
    Paired,
    /// The code ran out.
    Expired,
    /// Wrong code.
    Invalid,
    /// Too many wrong tries; stay silent.
    RateLimited,
}

struct Inner {
    file: StateFile,
    invites: PairingInvites<String>,
    refused: HashMap<String, Instant>,
    refusal_times: VecDeque<Instant>,
}

/// Durable adapter state behind one lock.
pub struct DiscordState {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

fn six_digit_code() -> String {
    const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
    loop {
        let b = *Uuid::new_v4().as_bytes();
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        if n < LIMIT {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

impl DiscordState {
    /// State that is lost when the program ends (tests, no data dir).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::with(None, StateFile::default())
    }

    /// Open (or start) the state file at `path`.
    ///
    /// # Errors
    /// The file exists but cannot be read or parsed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| DiscordError::State(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StateFile::default(),
            Err(e) => return Err(DiscordError::State(format!("{}: {e}", path.display()))),
        };
        Ok(Self::with(Some(path), file))
    }

    fn with(path: Option<PathBuf>, file: StateFile) -> Self {
        Self {
            path,
            inner: Mutex::new(Inner {
                file,
                invites: PairingInvites::default(),
                refused: HashMap::new(),
                refusal_times: VecDeque::new(),
            }),
        }
    }

    fn persist(path: Option<&Path>, file: &StateFile) -> Result<()> {
        let Some(path) = path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| DiscordError::State(e.to_string()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text =
            serde_json::to_string_pretty(file).map_err(|e| DiscordError::State(e.to_string()))?;
        std::fs::write(&tmp, text).map_err(|e| DiscordError::State(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| DiscordError::State(e.to_string()))
    }

    /// A fresh one-time code (replaces any older live one).
    #[must_use]
    pub fn create_code(&self) -> String {
        let code = six_digit_code();
        let now = Instant::now();
        self.inner
            .lock()
            .invites
            .create(code.clone(), PAIR_CODE_TTL, now);
        code
    }

    /// Invalidate a real code disclosed in a server before anyone can redeem
    /// it privately. Checking wrong codes shares the normal guessing budget.
    pub fn reject_public_code(&self, code: &str) {
        let mut g = self.inner.lock();
        // A previous sender's lockout must never keep a disclosed code alive.
        // This synthetic identity can only consume an invite, never pair.
        let sender = format!("public:{}", Uuid::new_v4());
        if let PairingMatch::Valid(ticket) = g.invites.check(sender.clone(), code, Instant::now()) {
            g.invites.consume(&sender, ticket);
        }
    }

    /// Try `code` for `user_id`. On success the account becomes the owner, the
    /// allowed server channels of any earlier owner are forgotten, and the
    /// file is written before the code is used up.
    ///
    /// # Errors
    /// The state file cannot be written; the code stays usable.
    pub fn redeem(&self, user_id: &str, code: &str) -> Result<PairOutcome> {
        let now = Instant::now();
        let mut g = self.inner.lock();
        match g.invites.check(user_id.to_owned(), code, now) {
            PairingMatch::Valid(ticket) => {
                let mut next = g.file.clone();
                if next.owner_id.as_deref() != Some(user_id) {
                    next.allowed_channels.clear();
                    next.bots.clear();
                    next.threads.clear();
                }
                next.owner_id = Some(user_id.to_owned());
                Self::persist(self.path.as_deref(), &next)?;
                g.file = next;
                g.invites.consume(&user_id.to_owned(), ticket);
                Ok(PairOutcome::Paired)
            }
            PairingMatch::Expired => Ok(PairOutcome::Expired),
            PairingMatch::InvalidCode => Ok(PairOutcome::Invalid),
            PairingMatch::RateLimited => Ok(PairOutcome::RateLimited),
        }
    }

    /// The paired owner's account id.
    #[must_use]
    pub fn owner(&self) -> Option<String> {
        self.inner.lock().file.owner_id.clone()
    }

    /// Is `user_id` the paired owner?
    #[must_use]
    pub fn is_owner(&self, user_id: &str) -> bool {
        self.inner.lock().file.owner_id.as_deref() == Some(user_id)
    }

    /// Is `id` the paired owner's account? The comparison is of the numeric ids only.
    #[must_use]
    pub fn is_owner_id(&self, id: OwnerId) -> bool {
        self.is_owner(&id.get().to_string())
    }

    /// Number of paired accounts (0 or 1).
    #[must_use]
    pub fn paired_count(&self) -> usize {
        usize::from(self.owner().is_some())
    }

    /// Interface language code.
    #[must_use]
    pub fn language(&self) -> Option<String> {
        self.inner.lock().file.language.clone()
    }

    fn update(&self, f: impl FnOnce(&mut StateFile)) -> Result<()> {
        let mut g = self.inner.lock();
        let mut next = g.file.clone();
        f(&mut next);
        Self::persist(self.path.as_deref(), &next)?;
        g.file = next;
        Ok(())
    }

    /// Remember the interface language.
    ///
    /// # Errors
    /// The state file cannot be written.
    pub fn set_language(&self, code: &str) -> Result<()> {
        self.update(|f| f.language = Some(code.to_owned()))
    }

    /// May the bot answer in this server channel?
    #[must_use]
    pub fn is_allowed_channel(&self, channel_id: &str) -> bool {
        self.inner.lock().file.allowed_channels.contains(channel_id)
    }

    /// Allow or stop allowing a server channel.
    ///
    /// # Errors
    /// The state file cannot be written.
    pub fn set_channel_allowed(&self, channel_id: &str, allowed: bool) -> Result<()> {
        self.update(|f| {
            if allowed {
                f.allowed_channels.insert(channel_id.to_owned());
            } else {
                f.allowed_channels.remove(channel_id);
            }
        })
    }

    /// Bot chosen for a Discord channel.
    #[must_use]
    pub fn bot_for(&self, channel_id: &str) -> Option<BotId> {
        self.inner.lock().file.bots.get(channel_id).copied()
    }

    /// Choose the bot for a Discord channel.
    ///
    /// # Errors
    /// The state file cannot be written.
    pub fn set_bot_for(&self, channel_id: &str, bot: BotId) -> Result<()> {
        self.update(|f| {
            f.bots.insert(channel_id.to_owned(), bot);
        })
    }

    /// Daemon chat used for a channel and bot.
    #[must_use]
    pub fn thread_for(&self, channel_id: &str, bot: BotId) -> Option<ChatId> {
        self.inner
            .lock()
            .file
            .threads
            .get(&format!("{channel_id}:{bot}"))
            .copied()
    }

    /// Remember the daemon chat for a channel and bot.
    ///
    /// # Errors
    /// The state file cannot be written.
    pub fn set_thread_for(&self, channel_id: &str, bot: BotId, chat: ChatId) -> Result<()> {
        self.update(|f| {
            f.threads.insert(format!("{channel_id}:{bot}"), chat);
        })
    }

    /// Should this stranger get a polite refusal now? At most one per account
    /// per day and a small number per minute overall.
    #[must_use]
    pub fn should_refuse(&self, user_id: &str) -> bool {
        let now = Instant::now();
        let mut g = self.inner.lock();
        if g.refused.len() >= MAX_TRACKED {
            g.refused
                .retain(|_, t| now.duration_since(*t) < REFUSAL_COOLDOWN);
        }
        if g.refused
            .get(user_id)
            .is_some_and(|t| now.duration_since(*t) < REFUSAL_COOLDOWN)
        {
            return false;
        }
        while g
            .refusal_times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            g.refusal_times.pop_front();
        }
        if g.refusal_times.len() >= MAX_REFUSALS_PER_MINUTE {
            return false;
        }
        g.refusal_times.push_back(now);
        g.refused.insert(user_id.to_owned(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_expires_locks_out_and_forgets_previous_owner_channels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = DiscordState::open(&path).unwrap();
        state.inner.lock().invites.create(
            "123456".into(),
            Duration::from_secs(1),
            Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
        );
        assert_eq!(state.redeem("42", "123456").unwrap(), PairOutcome::Expired);
        let code = state.create_code();
        for _ in 0..5 {
            let _ = state.redeem("99", "wrong").unwrap();
        }
        assert_eq!(state.redeem("99", &code).unwrap(), PairOutcome::RateLimited);
        let code = state.create_code();
        assert_eq!(state.redeem("42", &code).unwrap(), PairOutcome::Paired);
        state.set_channel_allowed("666", true).unwrap();
        let code = state.create_code();
        assert_eq!(state.redeem("100", &code).unwrap(), PairOutcome::Paired);
        let reopened = DiscordState::open(&path).unwrap();
        assert!(reopened.is_owner("100"));
        assert!(!reopened.is_owner("42"));
        assert!(!reopened.is_allowed_channel("666"));
        let code = state.create_code();
        state.reject_public_code(&code);
        assert_eq!(state.redeem("42", &code).unwrap(), PairOutcome::Invalid);
    }
}
