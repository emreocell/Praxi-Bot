//! The daemon's background loops under watch: what the owner is told when one crashes, and the
//! state `GET /v1/health` and the doctor show.
//!
//! The loops themselves (the scheduler, the mission wake loop, the mission drivers) are kept
//! alive by [`liveness::Guard`]; this module only turns what the guard sees into an owner notice
//! and a health field.

use channel_api::notice::{NoticeKind, NoticeSource};
use liveness::{EventKind, LoopEvent, LoopState};

use crate::AppState;
use crate::dto::{BackgroundHealth, BackgroundLoop};
use crate::owner_notify::Draft;

/// What the owner calls a loop.
fn display_name(loop_name: &str) -> &str {
    match loop_name {
        scheduler::LOOP_NAME => "Schedules",
        missions::supervisor::WAKE_LOOP => "Mission wake-ups",
        missions::supervisor::DRIVER_LOOP => "Missions",
        other => other,
    }
}

/// The owner notice for a loop that crashed (fixed text: a panic message may carry a path).
pub(crate) fn loop_notice(event: &LoopEvent) -> Draft {
    let what = display_name(event.name);
    let (title, reason) = match event.kind {
        EventKind::Restarted => (
            format!("{what} stopped and were restarted"),
            "A background task of the daemon crashed and was started again by itself. If this notice comes back, open Settings > Doctor.".to_owned(),
        ),
        EventKind::GaveUp => (
            format!("{what} are not running"),
            format!(
                "A background task of the daemon crashed {} times in a row and was stopped. Restart the daemon to bring it back; Settings > Doctor shows the state.",
                event.crashes
            ),
        ),
    };
    let mut draft = Draft::new(NoticeKind::Failed, NoticeSource::Run, title);
    draft.reason = Some(reason);
    draft
}

impl AppState {
    /// The guard that keeps the background loops alive.
    #[must_use]
    pub fn loop_guard(&self) -> &liveness::Guard {
        &self.0.loop_guard
    }

    /// Send a crash of a loop to the owner. Called once while the state is built.
    pub(crate) fn wire_loop_notices(&self) {
        let weak = self.downgrade();
        self.0
            .loop_guard
            .set_notifier(std::sync::Arc::new(move |event| {
                if let Some(state) = weak.upgrade() {
                    state
                        .owner_notify()
                        .publish(loop_notice(event), storage::now_ms());
                }
            }));
    }

    /// The loops and whether they are running, for `GET /v1/health`.
    pub(crate) fn background_health(&self) -> BackgroundHealth {
        let loops: Vec<BackgroundLoop> = self
            .0
            .loop_guard
            .statuses()
            .into_iter()
            .map(|s| BackgroundLoop {
                name: s.name.to_owned(),
                degraded: s.state == LoopState::Degraded,
                restarts: s.restarts,
            })
            .collect();
        BackgroundHealth {
            status: if loops.iter().any(|l| l.degraded) {
                "degraded"
            } else {
                "ok"
            }
            .to_owned(),
            loops,
        }
    }
}
