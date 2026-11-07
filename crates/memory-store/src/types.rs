//! Public data types.

use protocol::{BotId, ChatId, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name { $($(#[$vm])* $var),* }
        impl $name {
            /// Database/wire string.
            #[must_use]
            pub fn as_str(self) -> &'static str { match self { $(Self::$var => $s),* } }
            /// Parse the database/wire string.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> { match s { $($s => Some(Self::$var),)* _ => None } }
        }
    };
}

str_enum!(
    /// What a memory is about.
    Kind {
        /// Facts about the user.
        Profile = "profile",
        /// Facts about / preferences for one bot.
        Bot = "bot",
        /// Something that happened.
        Episodic = "episodic",
        /// How to do something.
        Procedural = "procedural",
        /// Learned from a correction.
        Lesson = "lesson",
    }
);

str_enum!(
    /// Provenance trust.
    Trust {
        /// Stated by the user.
        User = "user",
        /// Came from web/file/tool content; rendered as data only.
        Untrusted = "untrusted",
    }
);

str_enum!(
    /// Journal operation.
    JournalOp {
        /// Memory created.
        Add = "add",
        /// Memory changed.
        Update = "update",
        /// Memory soft-deleted.
        Delete = "delete",
    }
);

/// A stored memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Public id.
    pub id: Uuid,
    /// Owner.
    pub user_id: UserId,
    /// `None` = shared across the user's bots.
    pub bot_id: Option<BotId>,
    /// Category.
    pub kind: Kind,
    /// The remembered statement.
    pub content: String,
    /// Verbatim supporting quote.
    pub source_quote: Option<String>,
    /// Chat it came from.
    pub source_chat_id: Option<ChatId>,
    /// Message it came from.
    pub source_message_id: Option<Uuid>,
    /// Provenance trust.
    pub trust: Trust,
    /// 0..=1.
    pub importance: f32,
    /// Creation time (ms).
    pub created_ms: i64,
    /// Last time recall returned it (ms).
    pub last_used_ms: i64,
    /// Expiry (ms), if any.
    pub valid_until_ms: Option<i64>,
    /// Soft-delete flag.
    pub deleted: bool,
}

/// Input for [`MemoryStore::add`](crate::MemoryStore::add).
#[derive(Debug, Clone, PartialEq)]
pub struct NewMemory {
    /// Owner.
    pub user_id: UserId,
    /// Bot scope; `None` = shared.
    pub bot_id: Option<BotId>,
    /// Category.
    pub kind: Kind,
    /// Statement.
    pub content: String,
    /// Supporting quote.
    pub source_quote: Option<String>,
    /// Source chat.
    pub source_chat_id: Option<ChatId>,
    /// Source message.
    pub source_message_id: Option<Uuid>,
    /// Trust.
    pub trust: Trust,
    /// 0..=1.
    pub importance: f32,
    /// Expiry.
    pub valid_until_ms: Option<i64>,
}

impl NewMemory {
    /// Minimal user-trusted memory with default importance.
    #[must_use]
    pub fn new(
        user_id: UserId,
        bot_id: Option<BotId>,
        kind: Kind,
        content: impl Into<String>,
    ) -> Self {
        Self {
            user_id,
            bot_id,
            kind,
            content: content.into(),
            source_quote: None,
            source_chat_id: None,
            source_message_id: None,
            trust: Trust::User,
            importance: 0.5,
            valid_until_ms: None,
        }
    }
}

/// Filter for [`MemoryStore::list`](crate::MemoryStore::list).
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Restrict to memories visible to this bot (shared + that bot's).
    pub bot_id: Option<BotId>,
    /// Restrict to a kind.
    pub kind: Option<Kind>,
    /// Include soft-deleted rows.
    pub include_deleted: bool,
}

/// One journal row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Journal id (pass to `undo`).
    pub id: i64,
    /// Memory concerned.
    pub memory_id: Uuid,
    /// Operation.
    pub op: JournalOp,
    /// Snapshot before (None for add).
    pub before: Option<Memory>,
    /// Snapshot after.
    pub after: Option<Memory>,
    /// Why (free text).
    pub reason: Option<String>,
    /// Time (ms).
    pub ts_ms: i64,
    /// Already undone.
    pub undone: bool,
}

/// Partial update for [`MemoryStore::update`](crate::MemoryStore::update); `None` = keep.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryPatch {
    /// New statement.
    pub content: Option<String>,
    /// New supporting quote.
    pub source_quote: Option<String>,
    /// New source chat.
    pub source_chat_id: Option<ChatId>,
    /// New source message.
    pub source_message_id: Option<Uuid>,
    /// New trust.
    pub trust: Option<Trust>,
    /// New importance.
    pub importance: Option<f32>,
    /// New expiry (`Some(None)` clears it).
    pub valid_until_ms: Option<Option<i64>>,
}
