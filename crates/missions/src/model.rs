//! The mission model: states, the ledger item, budgets and progress counts.
//!
//! Pure data and rules; no I/O. The transition table is the only place that
//! decides which state may follow which (like `task-runtime`'s run states).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest item key, in characters.
pub const MAX_KEY_CHARS: usize = 120;
/// Longest item title, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest item result / error / reason / answer text, in characters.
pub const MAX_TEXT_CHARS: usize = 600;
/// Largest item payload (compact JSON), in bytes.
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024;
/// Most items one mission may hold.
pub const MAX_ITEMS: usize = 500;
/// Most items one `mission_plan` call may add.
pub const MAX_ITEMS_PER_CALL: usize = 100;
/// Longest mission note, in characters.
pub const MAX_NOTE_CHARS: usize = 400;
/// Most notes one mission keeps (the oldest unpinned are dropped).
pub const MAX_NOTES: usize = 60;

/// Where a mission is in its life.
///
/// Wave 1 has no `draft`/`intake`/`waiting` states yet: a mission starts
/// `running` when the owner creates it (see `docs/design/missions-adr.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    /// There is work to do: a session is in flight or the next one is queued.
    Running,
    /// The owner paused it; no session starts.
    Paused,
    /// Nothing can move without the owner or a budget change.
    Blocked,
    /// The success criterion is met (checked by code).
    Done,
    /// Ended by an unrecoverable error.
    Failed,
    /// The owner stopped it.
    Cancelled,
}

impl MissionState {
    /// Stable wire / database name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// No further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for MissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the mission may move from `from` to `to`.
///
/// ```text
/// running --budget/deadline/stalled/model errors/owner needed--> blocked
/// blocked --owner acts (answer, raise budget, "continue")-------> running
/// running|blocked --owner pause--> paused --owner resume--> running
/// any non-terminal --owner stop--> cancelled
/// running|blocked --criterion met (code-checked)--> done
/// running|blocked|paused --unrecoverable--> failed
/// ```
#[must_use]
pub const fn can_transition(from: MissionState, to: MissionState) -> bool {
    use MissionState::{Blocked, Cancelled, Done, Failed, Paused, Running};
    matches!(
        (from, to),
        (Running, Blocked | Paused | Done | Failed | Cancelled)
            | (Blocked, Running | Paused | Done | Failed | Cancelled)
            | (Paused, Running | Failed | Cancelled)
    )
}

/// What one ledger item is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Not started (pending).
    Todo,
    /// A session is working on it.
    Doing,
    /// Finished successfully.
    Done,
    /// Parked: needs the owner (CAPTCHA, 2FA, a question). Others go on.
    Blocked,
    /// Gave up (the site refuses, needs payment, ...).
    Failed,
    /// Deliberately left out.
    Skipped,
}

impl ItemStatus {
    /// Stable wire / database name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "todo" => Self::Todo,
            "doing" => Self::Doing,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            _ => return None,
        })
    }

    /// The item still has work for a session (`todo` or `doing`).
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Todo | Self::Doing)
    }
}

impl fmt::Display for ItemStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Limits the supervisor enforces before every session. The model cannot
/// change them; only the owner can.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budgets {
    /// Most sessions (agent runs) over the mission's life.
    pub max_sessions: u32,
    /// Most model calls summed over all sessions.
    pub max_model_steps: u32,
    /// Wall clock from start to deadline, in minutes.
    pub max_minutes: u64,
    /// Model spend cap in USD; `None` = no mission cap (the bot's own spend limits still apply).
    pub max_cost_usd: Option<f64>,
    /// Sessions in a row without ledger progress before the mission blocks.
    pub max_stalled_sessions: u32,
    /// Sessions in a row that failed (model/provider errors) before the mission blocks.
    pub max_failed_sessions: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_sessions: 60,
            max_model_steps: 1500,
            max_minutes: 14 * 24 * 60,
            max_cost_usd: None,
            max_stalled_sessions: 4,
            max_failed_sessions: 3,
        }
    }
}

impl Budgets {
    /// Reject values that make no sense.
    ///
    /// # Errors
    /// What is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_sessions == 0 || self.max_model_steps == 0 || self.max_minutes == 0 {
            return Err("sessions, model steps and minutes must be at least 1".into());
        }
        if self.max_stalled_sessions == 0 || self.max_failed_sessions == 0 {
            return Err("stalled and failed session limits must be at least 1".into());
        }
        if self.max_cost_usd.is_some_and(|c| !c.is_finite() || c < 0.0) {
            return Err("the cost cap must be a non-negative number".into());
        }
        Ok(())
    }
}

/// One mission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mission {
    /// Primary key.
    pub id: String,
    /// Owner.
    pub user_id: String,
    /// The lead bot.
    pub bot_id: String,
    /// The mission's own chat (sessions run in it).
    pub chat_id: String,
    /// Short label.
    pub title: String,
    /// The owner's words, verbatim.
    pub goal_text: String,
    /// Success criterion: at least this many items `done`; `None` = all items finished.
    pub target_done: Option<u32>,
    /// Limits.
    pub budgets: Budgets,
    /// Current state.
    pub state: MissionState,
    /// Why it is in this state (for `blocked`, `failed`, `paused`).
    pub state_reason: Option<String>,
    /// Sessions started so far.
    pub sessions: u32,
    /// Model calls summed over sessions.
    pub model_steps: u32,
    /// Spend summed over sessions, in USD.
    pub cost_usd: f64,
    /// Sessions in a row without progress.
    pub stalled_sessions: u32,
    /// Sessions in a row that failed.
    pub failed_sessions: u32,
    /// Bumped by every ledger write.
    pub ledger_version: i64,
    /// Batched owner questions asked so far (the mission allows one batch).
    pub questions_asked: u32,
    /// The run of the session in flight, if any.
    pub lease_run_id: Option<String>,
    /// Owner digest, set when the mission ends or blocks.
    pub digest: Option<Digest>,
    /// Creation time.
    pub created_at_ms: i64,
    /// Start time (the deadline counts from here).
    pub started_at_ms: i64,
    /// Last change.
    pub updated_at_ms: i64,
    /// End time of a terminal mission.
    pub finished_at_ms: Option<i64>,
}

/// One ledger item: the unit of progress (a site to register on, a lead, a document).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Dedupe key, unique per mission (a domain, an address, a slug).
    pub key: String,
    /// Free label such as `site`, `lead`, `task`.
    pub kind: String,
    /// What to do, one line.
    pub title: String,
    /// Where it stands.
    pub status: ItemStatus,
    /// How many sessions worked on it and failed to finish.
    pub attempts: u32,
    /// Small structured facts for whoever picks it up.
    pub payload: Option<serde_json::Value>,
    /// What was achieved (evidence), when done.
    pub result: Option<String>,
    /// Last failure text.
    pub last_error: Option<String>,
    /// Why it is parked (blocked items).
    pub blocked_reason: Option<String>,
    /// What the owner answered for a blocked item.
    pub owner_answer: Option<String>,
    /// Last change.
    pub updated_at_ms: i64,
}

/// Item counts, the headline numbers of a mission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// All items.
    pub total: u32,
    /// `todo`.
    pub todo: u32,
    /// `doing`.
    pub doing: u32,
    /// `done`.
    pub done: u32,
    /// `blocked`.
    pub blocked: u32,
    /// `failed`.
    pub failed: u32,
    /// `skipped`.
    pub skipped: u32,
}

impl Progress {
    /// Items a session can still work on.
    #[must_use]
    pub const fn open(&self) -> u32 {
        self.todo + self.doing
    }

    /// Items that are neither done nor given up: still to be finished
    /// (`todo`, `doing`, `blocked`). This is the "left" number in the UI.
    #[must_use]
    pub const fn left(&self) -> u32 {
        self.todo + self.doing + self.blocked
    }

    /// The success criterion is met. With a target, `done >= target`; without
    /// one, every item is finished (nothing open and nothing blocked) and the
    /// ledger is not empty.
    #[must_use]
    pub fn criterion_met(&self, target_done: Option<u32>) -> bool {
        match target_done {
            Some(n) => self.done >= n && self.total > 0,
            None => self.total > 0 && self.left() == 0,
        }
    }
}

/// What the owner reads when a mission ends or blocks. Built by code from the
/// ledger, not written by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    /// When it was written.
    pub at_ms: i64,
    /// `done`, `blocked`, `failed`, `cancelled`.
    pub outcome: String,
    /// Why (the mission's state reason).
    pub reason: Option<String>,
    /// Headline numbers.
    pub progress: Progress,
    /// Items that need the owner: key, title and the question / reason.
    pub blocked: Vec<DigestItem>,
    /// Items given up, with the last error.
    pub failed: Vec<DigestItem>,
    /// A few finished items with their results.
    pub done_sample: Vec<DigestItem>,
    /// Decisions the bot recorded ("which email I used").
    pub decisions: Vec<String>,
    /// Sessions, model calls and spend.
    pub sessions: u32,
    /// Model calls.
    pub model_steps: u32,
    /// Spend in USD.
    pub cost_usd: f64,
}

/// One line of a [`Digest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestItem {
    /// Item key.
    pub key: String,
    /// Item title.
    pub title: String,
    /// Reason, error or result text.
    pub text: Option<String>,
}

/// A note the bot wrote down (decisions it made, things it learned).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// `decision`, `learned` or `warning`.
    pub kind: String,
    /// The text, capped.
    pub text: String,
    /// Shown in every brief.
    pub pinned: bool,
    /// When it was written.
    pub at_ms: i64,
}

/// Clip `s` to `max` characters (not bytes), adding an ellipsis when cut.
#[must_use]
pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [MissionState; 6] = [
        MissionState::Running,
        MissionState::Paused,
        MissionState::Blocked,
        MissionState::Done,
        MissionState::Failed,
        MissionState::Cancelled,
    ];

    #[test]
    fn terminal_states_have_no_exit() {
        for from in ALL.into_iter().filter(|s| s.is_terminal()) {
            for to in ALL {
                assert!(!can_transition(from, to), "{from} -> {to}");
            }
        }
    }

    #[test]
    fn transition_table_matches_the_adr() {
        use MissionState::{Blocked, Cancelled, Done, Failed, Paused, Running};
        let allowed = [
            (Running, Blocked),
            (Running, Paused),
            (Running, Done),
            (Running, Failed),
            (Running, Cancelled),
            (Blocked, Running),
            (Blocked, Paused),
            (Blocked, Done),
            (Blocked, Failed),
            (Blocked, Cancelled),
            (Paused, Running),
            (Paused, Failed),
            (Paused, Cancelled),
        ];
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    can_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for s in ALL {
            assert_eq!(MissionState::parse(s.as_str()), Some(s));
        }
        for s in [
            ItemStatus::Todo,
            ItemStatus::Doing,
            ItemStatus::Done,
            ItemStatus::Blocked,
            ItemStatus::Failed,
            ItemStatus::Skipped,
        ] {
            assert_eq!(ItemStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(MissionState::parse("waiting"), None);
    }

    #[test]
    fn criterion_with_and_without_target() {
        let p = Progress {
            total: 5,
            done: 3,
            blocked: 1,
            skipped: 1,
            ..Progress::default()
        };
        assert!(!p.criterion_met(None), "a blocked item is not finished");
        assert!(p.criterion_met(Some(3)));
        assert!(!p.criterion_met(Some(4)));
        assert!(
            !Progress::default().criterion_met(None),
            "an empty ledger is not done"
        );
        assert!(!Progress::default().criterion_met(Some(0)));
    }

    #[test]
    fn clip_counts_characters() {
        assert_eq!(clip("  abc ", 10), "abc");
        assert_eq!(clip("çğışöü", 4), "çğı…");
    }

    #[test]
    fn budgets_validate() {
        assert!(Budgets::default().validate().is_ok());
        let bad = Budgets {
            max_sessions: 0,
            ..Budgets::default()
        };
        assert!(bad.validate().is_err());
        let nan = Budgets {
            max_cost_usd: Some(f64::NAN),
            ..Budgets::default()
        };
        assert!(nan.validate().is_err());
    }
}
