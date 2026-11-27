//! Missions: durable long-running jobs that chain agent runs until the task is done.
//!
//! A mission is one owner request ("register me on backlink sites") executed as
//! a chain of short *sessions*. A session is an ordinary agent run whose only
//! memory is a code-built brief read from the mission's ledger. A deterministic
//! supervisor decides when the next session starts and what happens when one
//! ends (continue, finish, block). See `docs/design/missions-adr.md` for the
//! design and the waves this crate implements.
//!
//! * [`model`]: states, items, budgets, progress counts, the transition table.
//! * [`store`]: the SQLite ledger (`missions.db`).
//! * [`brief`]: the "where am I" message built for every session.
//! * [`digest`]: the owner digest built from the ledger.
//! * [`supervisor`]: starts the next session until the mission is done.
//! * [`tools`]: the `mission_*` tools the bot uses to keep the ledger.

pub mod brief;
pub mod digest;
pub mod error;
pub mod model;
pub mod store;
pub mod supervisor;
pub mod tools;

pub use error::{MissionError, Result};
pub use model::{
    Budgets, Digest, DigestItem, Item, ItemStatus, Mission, MissionState, Note, Progress,
    can_transition,
};
pub use store::{
    Event, ItemUpdate, Leftover, MissionStore, NewItem, NewMission, SessionEndRecord, UpsertReport,
};

/// File name of the mission database inside the data directory.
pub const DB_FILE: &str = "missions.db";

/// Prefix of the run `kind` of a mission session: `turn:mission:<mission id>`.
pub const RUN_KIND_PREFIX: &str = "turn:mission:";

/// The run kind of a session of mission `id`.
#[must_use]
pub fn run_kind(id: &str) -> String {
    format!("{RUN_KIND_PREFIX}{id}")
}

/// The mission a run kind belongs to, if it is a mission session.
#[must_use]
pub fn mission_of_kind(kind: &str) -> Option<&str> {
    kind.strip_prefix(RUN_KIND_PREFIX)
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_kind_round_trips() {
        assert_eq!(mission_of_kind(&run_kind("abc")), Some("abc"));
        assert_eq!(mission_of_kind("turn"), None);
        assert_eq!(mission_of_kind("turn:mission:"), None);
        assert_eq!(mission_of_kind("turn:schedule:abc"), None);
    }
}
