//! When the floating widget is on screen.
//!
//! One policy, applied by every visibility update (state events, the post-ack linger
//! timer and a user dismissal), always evaluated on the main thread against the CURRENT
//! control state:
//! - a run in progress shows it;
//! - a user Stop that is not acknowledged yet keeps it shown ("Stopping…"), unless the
//!   user dismissed that Stop's indicator (the main window keeps showing the truth);
//! - after an ack it stays for a short confirmation, measured from the NEWEST ack;
//! - it never covers the main window: while the main window is the focused, visible
//!   window the widget stays hidden (the main window shows the same status itself, in
//!   its run bar), and it reappears when the user leaves the main window.

use std::time::{Duration, Instant};

use crate::control::ControlMode;

/// How long the widget keeps showing a confirmed Stop after the most recent ack.
pub const CONFIRM_LINGER: Duration = Duration::from_millis(4200);

#[derive(Debug, Default)]
pub struct WidgetVisibility {
    /// Shown until this instant after the most recent ack (its confirmation).
    linger_until: Option<Instant>,
    /// The user dismissed the pending-Stop indicator for every sequence up to this one.
    dismissed_through: u64,
    /// Raised by every ack; a linger timer only acts for its own (newest) ack.
    generation: u64,
}

impl WidgetVisibility {
    /// Whether the widget should be shown now.
    #[must_use]
    pub fn visible(&self, mode: ControlMode, stop_seq: u64, acked: u64, now: Instant) -> bool {
        if mode != ControlMode::Idle {
            return true;
        }
        if stop_seq > acked && stop_seq > self.dismissed_through {
            return true;
        }
        self.linger_until.is_some_and(|until| now < until)
    }

    /// An ack arrived: start (or restart) the confirmation linger. Returns the timer's
    /// generation; an older timer can no longer hide anything.
    pub fn on_ack(&mut self, now: Instant) -> u64 {
        self.generation += 1;
        self.linger_until = Some(now + CONFIRM_LINGER);
        self.generation
    }

    /// The user dismissed the pending indicator (nothing is acknowledged by this).
    pub fn on_dismiss(&mut self, stop_seq: u64) {
        self.dismissed_through = self.dismissed_through.max(stop_seq);
        self.linger_until = None;
    }

    /// The linger timer of `generation` fired: hide only if it belongs to the newest
    /// ack and nothing keeps the widget visible right now.
    #[must_use]
    pub fn should_hide_on_timer(
        &self,
        generation: u64,
        mode: ControlMode,
        stop_seq: u64,
        acked: u64,
        now: Instant,
    ) -> bool {
        generation == self.generation && !self.visible(mode, stop_seq, acked, now)
    }
}

/// The widget is for when the main window is NOT in front: it floats over the user's other
/// work. When the main window is focused (and not minimized) its own run bar and Stop status
/// carry everything, so the always-on-top widget would only sit on top of the chat and the
/// right panel.
#[must_use]
pub fn presentable(policy_visible: bool, main_in_front: bool) -> bool {
    policy_visible && !main_in_front
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: ControlMode = ControlMode::Idle;

    #[test]
    fn the_widget_never_covers_the_main_window_while_it_is_in_front() {
        assert!(presentable(true, false), "run active, user elsewhere");
        assert!(
            !presentable(true, true),
            "main window focused: it carries the status"
        );
        assert!(!presentable(false, false));
        assert!(!presentable(false, true));
    }

    #[test]
    fn a_run_in_progress_shows_it_and_an_acked_idle_hides_it() {
        let v = WidgetVisibility::default();
        let now = Instant::now();
        assert!(v.visible(ControlMode::BotDriving, 0, 0, now));
        assert!(v.visible(ControlMode::HumanDriving, 0, 0, now));
        assert!(!v.visible(IDLE, 0, 0, now));
        assert!(!v.visible(IDLE, 3, 3, now));
    }

    #[test]
    fn an_unacknowledged_stop_stays_visible_until_the_user_dismisses_it() {
        let mut v = WidgetVisibility::default();
        let t0 = Instant::now();
        // daemon gone: no ack ever arrives
        assert!(v.visible(IDLE, 1, 0, t0));
        assert!(v.visible(IDLE, 1, 0, t0 + Duration::from_secs(3600)));
        v.on_dismiss(1);
        assert!(!v.visible(IDLE, 1, 0, t0 + Duration::from_secs(3600)));
        // a NEW Stop shows it again
        assert!(v.visible(IDLE, 2, 0, t0 + Duration::from_secs(3601)));
    }

    #[test]
    fn the_linger_is_measured_from_the_newest_ack() {
        let mut v = WidgetVisibility::default();
        let t0 = Instant::now();
        let g1 = v.on_ack(t0); // ack 1
        let t4 = t0 + Duration::from_secs(4);
        let g2 = v.on_ack(t4); // Stop 2 acked 4 s later
        // timer 1 fires 4.2 s after ack 1, i.e. 0.2 s after ack 2: must not hide
        let t42 = t0 + CONFIRM_LINGER;
        assert!(!v.should_hide_on_timer(g1, IDLE, 2, 2, t42));
        assert!(v.visible(IDLE, 2, 2, t42), "still in ack 2's confirmation");
        // timer 2 hides ~4.2 s after ack 2
        assert!(v.should_hide_on_timer(g2, IDLE, 2, 2, t4 + CONFIRM_LINGER));
    }

    #[test]
    fn a_new_stop_racing_an_old_hide_timer_is_not_hidden() {
        let mut v = WidgetVisibility::default();
        let t0 = Instant::now();
        let g1 = v.on_ack(t0);
        // Stop 2 is pressed before timer 1 fires and is not acked yet
        assert!(!v.should_hide_on_timer(g1, IDLE, 2, 1, t0 + CONFIRM_LINGER));
    }

    #[test]
    fn any_idle_update_during_the_linger_keeps_the_confirmation_visible() {
        let mut v = WidgetVisibility::default();
        let t0 = Instant::now();
        v.on_ack(t0);
        assert!(v.visible(IDLE, 1, 1, t0 + Duration::from_secs(1)));
        assert!(!v.visible(IDLE, 1, 1, t0 + CONFIRM_LINGER));
    }
}
