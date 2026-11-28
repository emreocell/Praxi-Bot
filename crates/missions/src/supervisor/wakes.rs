//! The wake loop of the supervisor: timers, timeouts, mailboxes and webhooks of items that
//! parked themselves with `mission_wait`.
//!
//! It runs wherever the supervisor runs, which is the leader only. One pass looks at everything
//! that is due, fires what happened, times out what ran out, and moves a `waiting` mission back to
//! `running` (at most [`WAKE_RELEASES_PER_HOUR`] times an hour per mission) so the drive loop
//! starts its next session. All of it reads the injected clock, so tests run it on a virtual one.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Supervisor, budget_block};
use crate::error::Result;
use crate::model::{MAX_WAKE_PAYLOAD_CHARS, MissionState};
use crate::wake::{
    MAIL_POLL_BACKOFF_MAX_SECS, MAIL_POLL_SECS, MAX_PROBES_PER_PASS, MailCheck, PRUNE_EVERY_MS,
    WAKE_RELEASES_PER_HOUR, WAKE_RETENTION_MS, Wait, WaitSpec, WakeReason, clean_external,
};

/// One look into a mailbox may take this long before it counts as a failure.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// What one pass of the wake loop did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WakePass {
    /// Waits that fired (a timer, a mail).
    pub fired: u32,
    /// Waits that ran out.
    pub timed_out: u32,
    /// Looks into a mailbox.
    pub probes: u32,
    /// Missions moved back to `running`.
    pub released: u32,
    /// Parked live hand-overs that ran out, or that nothing waits on any more.
    pub handovers_expired: u32,
}

/// What a mailbox look decided about a wait.
enum Looked {
    /// The mail is there: the wait fires with this summary.
    Hit(Value),
    /// Not yet.
    Nothing,
    /// The look failed (reason): the wait goes on, slower.
    Failed(String),
    /// The daemon is shutting down.
    Stop,
}

impl Supervisor {
    /// Run [`Self::wake_pass`] every `wake_poll` until shutdown.
    pub(super) async fn wake_loop(&self) {
        while !self.shutdown.is_cancelled() {
            if let Err(e) = self.wake_pass().await {
                tracing::warn!("missions: wake pass failed: {e}");
            }
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(self.cfg.wake_poll) => {}
            }
        }
    }

    /// One pass: fire what is due, time out what ran out, look into mailboxes, enforce the
    /// deadline of waiting missions and release the ones that have something to do.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn wake_pass(&self) -> Result<WakePass> {
        let now = self.clock.now_ms();
        let mut pass = WakePass::default();
        let mut touched: BTreeSet<String> = BTreeSet::new();

        // An item that waits for nothing would wait for ever.
        self.store.requeue_orphan_waiting(now)?;
        self.prune_old(now);

        // A CAPTCHA nobody finished on the page in time: its item fails, the page is let go.
        for h in self.store.expire_live_handovers(now)? {
            pass.handovers_expired += 1;
            self.handover_ended(&h, false);
        }
        // One whose item the owner settled by hand, or whose mission ended: nothing waits on
        // its page any more, so the bot's browser is not held (or locked) for it.
        for (h, give_back) in self.store.settle_orphan_live_handovers()? {
            pass.handovers_expired += 1;
            self.handover_ended(&h, give_back);
        }

        for w in self.store.due_timers(now)? {
            if self
                .store
                .fire_wait(w.id, WakeReason::Timer, None, now)?
                .is_some()
            {
                pass.fired += 1;
                touched.insert(w.mission_id);
            }
        }

        // Mailboxes: each wait is looked at most once a minute, a few per pass.
        let probe = self.mail.lock().clone();
        if let Some(probe) = &probe {
            for (w, bot) in self.store.mail_waits_to_poll(now, MAX_PROBES_PER_PASS)? {
                pass.probes += 1;
                match self.look(probe.as_ref(), &w, &bot, now).await {
                    Looked::Stop => return Ok(pass),
                    Looked::Hit(summary) => {
                        if self.fire_mail(&w, &summary, now)? {
                            pass.fired += 1;
                            touched.insert(w.mission_id.clone());
                        }
                    }
                    Looked::Nothing => {
                        self.store
                            .note_poll(w.id, now + MAIL_POLL_SECS * 1000, None)?;
                    }
                    Looked::Failed(why) => {
                        let gap = if w.last_error.is_some() {
                            MAIL_POLL_BACKOFF_MAX_SECS
                        } else {
                            MAIL_POLL_SECS * 2
                        };
                        self.store.note_poll(w.id, now + gap * 1000, Some(&why))?;
                    }
                }
            }
        }

        // What ran out. A mail wait gets one last look first: the mail may have come since the
        // previous one.
        for w in self.store.expired_waits(now)? {
            if let (WaitSpec::Mail { .. }, Some(probe)) = (&w.spec, &probe)
                && let Some(bot) = self.store.get(&w.mission_id)?.map(|m| m.bot_id)
                && let Looked::Hit(summary) = self.look(probe.as_ref(), &w, &bot, now).await
            {
                if self.fire_mail(&w, &summary, now)? {
                    pass.fired += 1;
                    touched.insert(w.mission_id.clone());
                }
                continue;
            }
            if self.store.time_out_wait(w.id, now)? {
                pass.timed_out += 1;
                touched.insert(w.mission_id);
            }
        }

        // A waiting mission still has a deadline and the other budgets.
        for m in self.store.list_by_state(MissionState::Waiting)? {
            if let Some(reason) = budget_block(&m, now) {
                self.block(&m.id, &reason).await?;
            }
        }

        // Release: a mission with something to do again (or nothing left to wait for).
        for id in self.store.waiting_to_release()? {
            if self
                .store
                .release_waiting(&id, WAKE_RELEASES_PER_HOUR, now)?
            {
                pass.released += 1;
                touched.insert(id);
            }
        }
        for id in &touched {
            self.changed(id);
        }
        if pass.released > 0 {
            self.wake();
        }
        Ok(pass)
    }

    /// Delete what ended long ago (at most once an hour): waits that fired, timed out or were
    /// cancelled and wakes that were delivered stay readable for [`WAKE_RETENTION_MS`], then go.
    fn prune_old(&self, now: i64) {
        let last = self.last_prune_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < PRUNE_EVERY_MS {
            return;
        }
        self.last_prune_ms.store(now, Ordering::Relaxed);
        if let Err(e) = self.store.prune_wakes(now, WAKE_RETENTION_MS) {
            tracing::warn!("missions: pruning old waits failed: {e}");
        }
    }

    /// A webhook the owner created was called (the daemon verified its signature): every wait on
    /// it fires with `body`, which is untrusted text. Returns how many missions were woken.
    ///
    /// # Errors
    /// Storage failures.
    pub fn deliver_webhook(&self, bot_id: &str, trigger_id: &str, body: &str) -> Result<u32> {
        let now = self.clock.now_ms();
        let woken = self.store.deliver_webhook(bot_id, trigger_id, body, now)?;
        let mut released = 0;
        for id in &woken {
            if self
                .store
                .release_waiting(id, WAKE_RELEASES_PER_HOUR, now)?
            {
                released += 1;
            }
            self.changed(id);
        }
        if released > 0 {
            self.wake();
        }
        Ok(u32::try_from(woken.len()).unwrap_or(u32::MAX))
    }

    async fn look(
        &self,
        probe: &dyn crate::wake::MailProbe,
        w: &Wait,
        bot: &str,
        now: i64,
    ) -> Looked {
        let WaitSpec::Mail { filter } = &w.spec else {
            return Looked::Nothing;
        };
        let check = MailCheck {
            bot_id: bot,
            filter,
            registered_at_ms: w.created_at_ms,
            now_ms: now,
        };
        tokio::select! {
            () = self.shutdown.cancelled() => Looked::Stop,
            r = tokio::time::timeout(PROBE_TIMEOUT, probe.check(&check)) => match r {
                Ok(Ok(Some(hit))) => Looked::Hit(hit.summary),
                Ok(Ok(None)) => Looked::Nothing,
                Ok(Err(why)) => Looked::Failed(clean_external(&why, 200)),
                Err(_) => Looked::Failed("the mailbox did not answer in time".to_owned()),
            },
        }
    }

    /// The expected mail came: record the wake with its (capped) summary.
    fn fire_mail(&self, w: &Wait, summary: &Value, now: i64) -> Result<bool> {
        let text = summary.to_string();
        let mut payload = if text.chars().count() > MAX_WAKE_PAYLOAD_CHARS {
            json!({"summary": clean_external(&text, MAX_WAKE_PAYLOAD_CHARS)})
        } else {
            summary.clone()
        };
        // The mailbox does not tell whether SPF/DKIM passed: arrival is not authenticity.
        if let Some(o) = payload.as_object_mut() {
            o.insert("sender_verified".into(), json!(false));
        }
        Ok(self
            .store
            .fire_wait(w.id, WakeReason::Mail, Some(&payload), now)?
            .is_some())
    }
}
