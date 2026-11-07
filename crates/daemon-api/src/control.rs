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
    pending: HashMap<Uuid, oneshot::Sender<Answer>>,
}

/// State of the bridge; see the module docs.
pub struct ControlHub {
    cfg: ControlConfig,
    state: Mutex<State>,
    /// Open global event streams (the desktop app's).
    subscribers: AtomicUsize,
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
        }
    }

    /// Register the app as controller / refresh the heartbeat.
    pub fn put_state(&self, mode: ControlMode) {
        self.state.lock().registration = Some((mode, Instant::now()));
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
        // The human has the wheel: refuse without a round trip.
        if let Some(msg) = registration.and_then(|(m, _)| refused_while(m)) {
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
                match self.registration().and_then(|(m, _)| refused_while(m)) {
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
        assert!(out.is_ok(), "an expired registration no longer blocks");
        assert_eq!(published, 0);
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
}
