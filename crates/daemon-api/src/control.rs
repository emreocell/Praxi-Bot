//! The desktop control bridge, daemon side.
//!
//! Desktop actions of a bot run inside this process, but the on-screen overlay
//! (and the "human has the wheel" state) live in the Tauri desktop app.  The
//! [`ControlHub`] connects the two:
//!
//! * before every mutating desktop action the agent loop calls
//!   [`ControlGate::before_action`] on the hub;
//! * the hub answers at once when it can (no desktop app connected: proceed;
//!   the app reported `human_driving` / `paused`: refuse), otherwise it emits a
//!   `control_requested` event and waits for the app's
//!   `POST /v1/control/{request_id}`;
//! * the app registers as *controller* with `PUT /v1/control/state`, a
//!   heartbeat that expires after [`ControlConfig::heartbeat_ttl`].  Without an
//!   answer the hub proceeds after [`ControlConfig::proceed_timeout`] for an
//!   app that is merely connected, but refuses after
//!   [`ControlConfig::refuse_timeout`] for a registered controller.
//!
//! `paused` / `human_driving` are **latched** (SEC-07): once reported they
//! refuse every action until an explicit [`ControlHub::release`]
//! (`POST /v1/control/release`).  The heartbeat TTL only decides whether a
//! controller counts as live; losing the app (crash, restart, closed stream)
//! never silently resumes the bot.  No heartbeat releases it, `idle` /
//! `bot_driving` included (a restarted UI reports `idle`): only
//! `POST /v1/control/release` or a `PUT /v1/control/state` that carries
//! `release: true` does.  The latch is persisted in the data directory
//! ([`ControlHub::with_latch_file`]), so it survives a daemon restart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_core::{ControlGate, ControlRequest};
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::dto::{ControlMode, ControlVerdict};

/// Timing of the bridge.
#[derive(Debug, Clone, Copy)]
pub struct ControlConfig {
    /// Wait for the app's answer when it is connected but not registered, then
    /// proceed.
    pub proceed_timeout: Duration,
    /// Wait for the app's answer when it is registered, then refuse.
    pub refuse_timeout: Duration,
    /// How long a `PUT /v1/control/state` keeps the registration alive.
    pub heartbeat_ttl: Duration,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            proceed_timeout: Duration::from_secs(2),
            refuse_timeout: Duration::from_secs(10),
            heartbeat_ttl: Duration::from_secs(15),
        }
    }
}

struct Answer {
    verdict: ControlVerdict,
    reason: Option<String>,
}

#[derive(Default)]
struct State {
    /// Last reported mode and when.
    registration: Option<(ControlMode, Instant)>,
    /// `Paused` / `HumanDriving`, kept until [`ControlHub::release`] (no TTL).
    latched: Option<ControlMode>,
    pending: HashMap<Uuid, oneshot::Sender<Answer>>,
}

/// State of the bridge; see the module docs.
pub struct ControlHub {
    cfg: ControlConfig,
    state: Mutex<State>,
    /// Open global event streams (the desktop app's).
    subscribers: AtomicUsize,
    /// Where the latch is persisted (`None`: in memory only).
    latch_file: Option<std::path::PathBuf>,
}

/// Counts an open global event stream while it lives.
pub struct SubscriberGuard(std::sync::Arc<ControlHub>);

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.0.subscribers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Removes a pending request when the waiting action is dropped.
struct PendingGuard<'a> {
    hub: &'a ControlHub,
    id: Uuid,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.hub.state.lock().pending.remove(&self.id);
    }
}

fn refused_while(mode: ControlMode) -> Option<&'static str> {
    match mode {
        ControlMode::HumanDriving => Some(
            "The user has taken control of the desktop, so this action was not performed. \
             Do not retry it; tell the user you are waiting and ask them to give control back.",
        ),
        ControlMode::Paused => Some(
            "The user has paused desktop control for this bot, so this action was not performed. \
             Do not retry it; ask the user to resume.",
        ),
        ControlMode::Idle | ControlMode::BotDriving => None,
    }
}

impl ControlHub {
    /// A hub with the given timing.
    #[must_use]
    pub fn new(cfg: ControlConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(State::default()),
            subscribers: AtomicUsize::new(0),
            latch_file: None,
        }
    }

    /// A hub whose latch is persisted in `path` (atomic write) and restored
    /// from it.  An unreadable file latches `Paused` (fail closed).
    #[must_use]
    pub fn with_latch_file(cfg: ControlConfig, path: std::path::PathBuf) -> Self {
        let mut hub = Self::new(cfg);
        let latched = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mode = serde_json::from_str::<ControlMode>(text.trim())
                    .ok()
                    .filter(|m| refused_while(*m).is_some());
                if mode.is_none() {
                    eprintln!("daemon-api: WARNING: control latch file unreadable; staying paused");
                }
                Some(mode.unwrap_or(ControlMode::Paused))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                eprintln!(
                    "daemon-api: WARNING: control latch file unreadable ({e}); staying paused"
                );
                Some(ControlMode::Paused)
            }
        };
        hub.state.get_mut().latched = latched;
        hub.latch_file = Some(path);
        hub
    }

    /// Write (or remove) the latch file; best effort, reported on stderr.
    fn persist(&self, latched: Option<ControlMode>) {
        let Some(path) = &self.latch_file else { return };
        let result = match latched {
            Some(m) => serde_json::to_vec(&m)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    let tmp = path.with_extension("tmp");
                    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path))
                }),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        if let Err(e) = result {
            eprintln!("daemon-api: WARNING: could not persist the control latch: {e}");
        }
    }

    /// Register the app as controller / refresh the heartbeat.
    ///
    /// `Paused` / `HumanDriving` latch (see the module docs); no mode, and no
    /// TTL expiry, ever clears a latch here.
    pub fn put_state(&self, mode: ControlMode) {
        self.apply_state(mode, false);
    }

    /// [`ControlHub::put_state`]; with `release` an `Idle` / `BotDriving` mode
    /// also clears the latch (the user's explicit resume).
    pub fn apply_state(&self, mode: ControlMode, release: bool) {
        let mut st = self.state.lock();
        st.registration = Some((mode, Instant::now()));
        let before = st.latched;
        if refused_while(mode).is_some() {
            st.latched = Some(mode);
        } else if release {
            st.latched = None;
        }
        if st.latched != before {
            self.persist(st.latched);
        }
    }

    /// The latched `Paused` / `HumanDriving` mode, if any (never expires).
    #[must_use]
    pub fn latched(&self) -> Option<ControlMode> {
        self.state.lock().latched
    }

    /// Explicitly give the desktop back to the bots: clears the latch and
    /// returns the mode that was latched.  A registration that still reports
    /// the refused mode is reset to `idle`.
    pub fn release(&self) -> Option<ControlMode> {
        let mut st = self.state.lock();
        let was = st.latched.take();
        if was.is_some() {
            self.persist(None);
        }
        if let Some((m, at)) = st.registration
            && refused_while(m).is_some()
        {
            st.registration = Some((ControlMode::Idle, at));
        }
        was
    }

    /// The mode of a live registration and the time left, `None` when no app
    /// is registered or the heartbeat expired.
    #[must_use]
    pub fn registration(&self) -> Option<(ControlMode, Duration)> {
        let st = self.state.lock();
        let (mode, at) = st.registration?;
        let left = self.cfg.heartbeat_ttl.checked_sub(at.elapsed())?;
        (!left.is_zero()).then_some((mode, left))
    }

    /// Last reported mode, including an expired registration. Heartbeat expiry
    /// changes liveness, not whether the next heartbeat is a mode transition.
    #[must_use]
    pub fn reported_mode(&self) -> Option<ControlMode> {
        self.state.lock().registration.map(|(mode, _)| mode)
    }

    /// A desktop app is listening on the global event stream.
    #[must_use]
    pub fn app_connected(&self) -> bool {
        self.subscribers.load(Ordering::SeqCst) > 0
    }

    /// Note an open global event stream until the guard is dropped.
    #[must_use]
    pub fn subscribe(self: &std::sync::Arc<Self>) -> SubscriberGuard {
        self.subscribers.fetch_add(1, Ordering::SeqCst);
        SubscriberGuard(std::sync::Arc::clone(self))
    }

    /// Deliver the app's answer; `false` when the request is unknown (already
    /// answered or timed out).
    pub fn decide(
        &self,
        request_id: Uuid,
        verdict: ControlVerdict,
        reason: Option<String>,
    ) -> bool {
        match self.state.lock().pending.remove(&request_id) {
            Some(tx) => tx.send(Answer { verdict, reason }).is_ok(),
            None => false,
        }
    }
}

#[async_trait]
impl ControlGate for ControlHub {
    async fn before_action(
        &self,
        req: ControlRequest,
        publish: &(dyn Fn() + Send + Sync),
    ) -> Result<(), String> {
        let registration = self.registration();
        // The human has the wheel (latched, independent of any heartbeat or
        // subscriber): refuse without a round trip.
        if let Some(msg) = self.latched().and_then(refused_while) {
            return Err(msg.to_owned());
        }
        let registered = registration.is_some();
        if !registered && !self.app_connected() {
            // CLI only: nobody to show an overlay to.
            return Ok(());
        }

        let (tx, rx) = oneshot::channel();
        self.state.lock().pending.insert(req.request_id, tx);
        let _guard = PendingGuard {
            hub: self,
            id: req.request_id,
        };
        publish();

        let wait = if registered {
            self.cfg.refuse_timeout
        } else {
            self.cfg.proceed_timeout
        };
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(Answer {
                verdict: ControlVerdict::Proceed,
                ..
            })) => {
                // The human may have taken the wheel while the target was
                // highlighted.
                match self.latched().and_then(refused_while) {
                    Some(msg) => Err(msg.to_owned()),
                    None => Ok(()),
                }
            }
            Ok(Ok(Answer {
                verdict: ControlVerdict::Refuse,
                reason,
            })) => Err(reason.unwrap_or_else(|| {
                "The user stopped this desktop action, so it was not performed. \
                 Do not retry it; ask the user how to continue."
                    .to_owned()
            })),
            // The sender vanished (hub state dropped): treat like no answer.
            Ok(Err(_)) | Err(_) if !registered => Ok(()),
            Ok(Err(_)) | Err(_) => Err(format!(
                "The desktop app did not confirm this action within {} s, so it was not \
                 performed. You may retry once; if it keeps failing, tell the user.",
                wait.as_secs().max(1)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use protocol::{BotId, RunId};

    use super::*;

    fn req() -> ControlRequest {
        ControlRequest {
            request_id: Uuid::now_v7(),
            run_id: RunId::new(),
            bot_id: BotId::new(),
            rect: None,
            label: "Save".into(),
            action: "click".into(),
        }
    }

    fn fast() -> ControlConfig {
        ControlConfig {
            proceed_timeout: Duration::from_millis(80),
            refuse_timeout: Duration::from_millis(200),
            heartbeat_ttl: Duration::from_millis(150),
        }
    }

    /// Run the gate; `publish` counts calls and, when `answer` is set, answers
    /// the request from another task.
    async fn gate(
        hub: &Arc<ControlHub>,
        answer: Option<(ControlVerdict, Option<&str>)>,
    ) -> (Result<(), String>, usize, Duration) {
        let r = req();
        let id = r.request_id;
        let published = Arc::new(AtomicUsize::new(0));
        let p = Arc::clone(&published);
        let h = Arc::clone(hub);
        let reason = answer.and_then(|(_, r)| r).map(str::to_owned);
        let verdict = answer.map(|(v, _)| v);
        let publish = move || {
            p.fetch_add(1, Ordering::SeqCst);
            if let Some(v) = verdict {
                let (h, reason) = (Arc::clone(&h), reason.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    assert!(h.decide(id, v, reason));
                });
            }
        };
        let started = Instant::now();
        let out = hub.before_action(r, &publish).await;
        (out, published.load(Ordering::SeqCst), started.elapsed())
    }

    #[tokio::test]
    async fn no_app_connected_is_a_no_op() {
        let hub = Arc::new(ControlHub::new(fast()));
        let (out, published, took) = gate(&hub, None).await;
        assert!(out.is_ok());
        assert_eq!(published, 0, "no event without an app");
        assert!(took < Duration::from_millis(40));
    }

    #[tokio::test]
    async fn app_answers_proceed() {
        let hub = Arc::new(ControlHub::new(fast()));
        let _sub = hub.subscribe();
        hub.put_state(ControlMode::BotDriving);
        let (out, published, took) = gate(&hub, Some((ControlVerdict::Proceed, None))).await;
        assert!(out.is_ok());
        assert_eq!(published, 1);
        assert!(took < Duration::from_millis(150), "{took:?}");
        assert!(hub.state.lock().pending.is_empty(), "request cleaned up");
    }

    #[tokio::test]
    async fn app_answers_refuse_with_its_reason() {
        let hub = Arc::new(ControlHub::new(fast()));
        hub.put_state(ControlMode::BotDriving);
        let (out, _, _) = gate(
            &hub,
            Some((ControlVerdict::Refuse, Some("stopped by user"))),
        )
        .await;
        assert_eq!(out.unwrap_err(), "stopped by user");
    }

    #[tokio::test]
    async fn connected_but_unregistered_app_that_is_silent_proceeds_after_the_short_timeout() {
        let hub = Arc::new(ControlHub::new(fast()));
        let _sub = hub.subscribe();
        let (out, published, took) = gate(&hub, None).await;
        assert!(out.is_ok());
        assert_eq!(published, 1);
        assert!(took >= Duration::from_millis(70), "{took:?}");
        assert!(took < Duration::from_millis(190), "{took:?}");
    }

    #[tokio::test]
    async fn registered_controller_that_is_silent_is_refused_after_the_long_timeout() {
        let hub = Arc::new(ControlHub::new(ControlConfig {
            heartbeat_ttl: Duration::from_secs(5),
            ..fast()
        }));
        hub.put_state(ControlMode::BotDriving);
        let (out, published, took) = gate(&hub, None).await;
        let msg = out.unwrap_err();
        assert!(msg.contains("did not confirm"), "{msg}");
        assert_eq!(published, 1);
        assert!(took >= Duration::from_millis(190), "{took:?}");
    }

    #[tokio::test]
    async fn human_driving_and_paused_refuse_instantly_without_a_round_trip() {
        let hub = Arc::new(ControlHub::new(ControlConfig {
            heartbeat_ttl: Duration::from_secs(5),
            ..fast()
        }));
        let _sub = hub.subscribe();
        for (mode, needle) in [
            (ControlMode::HumanDriving, "taken control"),
            (ControlMode::Paused, "paused"),
        ] {
            hub.put_state(mode);
            let (out, published, took) = gate(&hub, None).await;
            let msg = out.unwrap_err();
            assert!(msg.contains(needle), "{msg}");
            assert_eq!(published, 0, "no event for {mode:?}");
            assert!(took < Duration::from_millis(40), "{mode:?} took {took:?}");
        }
    }

    #[tokio::test]
    async fn heartbeat_expires() {
        let hub = Arc::new(ControlHub::new(fast()));
        hub.put_state(ControlMode::HumanDriving);
        assert!(hub.registration().is_some());
        assert!(
            gate(&hub, None).await.0.is_err(),
            "refused while registered"
        );
        tokio::time::sleep(Duration::from_millis(180)).await;
        assert!(hub.registration().is_none(), "expired after the ttl");
        let (out, published, _) = gate(&hub, None).await;
        assert!(out.is_err(), "the latch outlives the heartbeat (SEC-07)");
        assert_eq!(published, 0);
        assert_eq!(hub.release(), Some(ControlMode::HumanDriving));
        let (out, _, _) = gate(&hub, None).await;
        assert!(
            out.is_ok(),
            "released: an expired registration does not block"
        );
        // A fresh heartbeat re-registers.
        hub.put_state(ControlMode::BotDriving);
        assert_eq!(
            hub.registration().map(|r| r.0),
            Some(ControlMode::BotDriving)
        );
    }

    #[tokio::test]
    async fn a_proceed_is_overruled_when_the_human_took_the_wheel_meanwhile() {
        let hub = Arc::new(ControlHub::new(ControlConfig {
            heartbeat_ttl: Duration::from_secs(5),
            ..fast()
        }));
        hub.put_state(ControlMode::BotDriving);
        let h = Arc::clone(&hub);
        let r = req();
        let id = r.request_id;
        let publish = move || {
            let h = Arc::clone(&h);
            tokio::spawn(async move {
                h.put_state(ControlMode::HumanDriving);
                h.decide(id, ControlVerdict::Proceed, None);
            });
        };
        let out = hub.before_action(r, &publish).await;
        assert!(out.unwrap_err().contains("taken control"));
    }

    #[test]
    fn unknown_request_ids_are_rejected() {
        let hub = ControlHub::new(fast());
        assert!(!hub.decide(Uuid::now_v7(), ControlVerdict::Proceed, None));
    }

    #[tokio::test]
    async fn pause_survives_the_heartbeat_ttl_and_only_release_resumes() {
        let hub = Arc::new(ControlHub::new(fast()));
        hub.put_state(ControlMode::Paused);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(hub.registration().is_none(), "heartbeat expired");
        let msg = gate(&hub, None).await.0.unwrap_err();
        assert!(msg.contains("paused"), "{msg}");
        // Another paused heartbeat keeps it latched.
        hub.put_state(ControlMode::Paused);
        assert!(gate(&hub, None).await.0.is_err());
        assert_eq!(hub.release(), Some(ControlMode::Paused));
        assert_eq!(hub.latched(), None);
        tokio::time::sleep(Duration::from_millis(200)).await; // heartbeat expires
        assert!(gate(&hub, None).await.0.is_ok());
        assert_eq!(hub.release(), None, "second release is a no-op");
    }

    #[tokio::test]
    async fn latched_pause_still_refuses_after_the_subscriber_is_dropped() {
        let hub = Arc::new(ControlHub::new(fast()));
        let sub = hub.subscribe();
        hub.put_state(ControlMode::HumanDriving);
        drop(sub);
        assert!(!hub.app_connected());
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (out, published, _) = gate(&hub, None).await;
        assert!(out.unwrap_err().contains("taken control"));
        assert_eq!(published, 0);
    }

    #[tokio::test]
    async fn only_an_explicit_release_clears_the_latch_no_heartbeat_does() {
        let hub = Arc::new(ControlHub::new(fast()));
        hub.put_state(ControlMode::Paused);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(gate(&hub, None).await.0.is_err(), "timeout keeps the latch");
        // A reopened UI heartbeats idle / bot_driving: the pause stays.
        hub.put_state(ControlMode::Idle);
        hub.put_state(ControlMode::BotDriving);
        hub.apply_state(ControlMode::Idle, false);
        assert_eq!(hub.latched(), Some(ControlMode::Paused));
        // The flag releases, but only with a non-refusing mode.
        hub.apply_state(ControlMode::Paused, true);
        assert_eq!(hub.latched(), Some(ControlMode::Paused));
        hub.apply_state(ControlMode::Idle, true);
        assert_eq!(hub.latched(), None);
    }

    #[test]
    fn the_latch_survives_a_restart_and_a_broken_file_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("control-latch.json");
        let hub = ControlHub::with_latch_file(fast(), f.clone());
        assert_eq!(hub.latched(), None);
        hub.put_state(ControlMode::HumanDriving);
        let again = ControlHub::with_latch_file(fast(), f.clone());
        assert_eq!(again.latched(), Some(ControlMode::HumanDriving));
        assert_eq!(again.release(), Some(ControlMode::HumanDriving));
        assert!(!f.exists());
        assert_eq!(
            ControlHub::with_latch_file(fast(), f.clone()).latched(),
            None
        );
        std::fs::write(&f, "garbage").unwrap();
        assert_eq!(
            ControlHub::with_latch_file(fast(), f).latched(),
            Some(ControlMode::Paused)
        );
    }

    #[test]
    fn expired_heartbeat_keeps_its_reported_mode_for_transition_auditing() {
        let hub = ControlHub::new(ControlConfig {
            heartbeat_ttl: Duration::ZERO,
            ..ControlConfig::default()
        });
        hub.put_state(ControlMode::Paused);
        assert!(hub.registration().is_none());
        assert_eq!(hub.reported_mode(), Some(ControlMode::Paused));
        assert_eq!(hub.latched(), Some(ControlMode::Paused));
    }
}
