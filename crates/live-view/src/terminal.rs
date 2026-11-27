//! A read-only look at the terminals the bot runs (`shell_interactive` sessions).
//!
//! A program on a pseudo-terminal has no window of its own, so the screen picture never shows it.
//! The tool that owns the sessions offers them here as masked text, and the live view lists them
//! in a panel next to the picture. Nothing in this module types into a session: the owner's
//! keyboard never reaches a terminal, in watching or in control.
use std::sync::Weak;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where a terminal session stands (the same four words the tool tells the bot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotTerminalState {
    Running,
    WaitingForInput,
    Finished,
    Failed,
}

/// One terminal session as the live view shows it. `command` and `screen` are already masked
/// (the vault's values and key shapes), exactly as the bot sees them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BotTerminal {
    pub id: String,
    pub bot_id: Option<Uuid>,
    pub command: String,
    pub state: BotTerminalState,
    pub exit_code: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    /// Seconds since the session started.
    pub age_secs: u64,
    /// The whole screen right now, blank rows at the end trimmed.
    pub screen: String,
}

/// What a tool that owns terminal sessions gives the live view. Reading must not count as use of
/// the session (it must not keep an idle one alive) and must never write to it.
pub trait BotTerminalSource: Send + Sync {
    fn terminals(&self) -> Vec<BotTerminal>;
}

/// The sources the live view reads, held weakly: dropping the tool still ends its sessions.
#[derive(Default)]
pub(crate) struct BotTerminalSources(std::sync::RwLock<Vec<Weak<dyn BotTerminalSource>>>);

impl BotTerminalSources {
    pub(crate) fn add(&self, source: Weak<dyn BotTerminalSource>) {
        let mut all = self.0.write().expect("terminal sources poisoned");
        all.retain(|s| s.strong_count() > 0);
        all.push(source);
    }

    pub(crate) fn list(&self) -> Vec<BotTerminal> {
        let sources: Vec<_> = self
            .0
            .read()
            .expect("terminal sources poisoned")
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut all: Vec<BotTerminal> = sources.iter().flat_map(|s| s.terminals()).collect();
        all.sort_by(|a, b| a.age_secs.cmp(&b.age_secs).then_with(|| a.id.cmp(&b.id)));
        all
    }
}
