//! Object-safe backend hooks. Dropping the last subscription ends capture.
use crate::{
    EffectiveProfile, FrameMetadata, InputEvent, MAX_PAYLOAD, TargetInfo, TargetUnavailable,
    ViewError,
};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

/// Encoded pixels are shared, never serialized into JSON or event logs.
#[derive(Clone)]
pub struct Frame {
    pub metadata: FrameMetadata,
    pub payload: Arc<[u8]>,
}
impl Frame {
    /// Validate before fan-out. Backends must also bound raw allocation to 16 Mpixel.
    /// # Errors
    /// Returns a fixed protocol error for invalid, unsupported, or oversized data.
    pub fn validate(&self) -> Result<(), ViewError> {
        let m = &self.metadata;
        if self.payload.is_empty()
            || self.payload.len() > MAX_PAYLOAD
            || m.width == 0
            || m.height == 0
            || !crate::wire_size_ok(m.width, m.height)
            || m.source_rect.width == 0
            || m.source_rect.height == 0
            || u64::from(m.source_rect.width) * u64::from(m.source_rect.height) > 16_000_000
            || !m.source_rect.x.is_finite()
            || !m.source_rect.y.is_finite()
        {
            return Err(ViewError::LimitExceeded);
        }
        Ok(())
    }
}

/// One latest pending frame; a slow reader cannot block the producer.
pub struct FrameSubscription {
    pub frames: watch::Receiver<Option<Arc<Frame>>>,
    on_drop: Option<Box<dyn FnOnce() + Send + Sync>>,
}
impl FrameSubscription {
    #[must_use]
    pub fn new(
        frames: watch::Receiver<Option<Arc<Frame>>>,
        on_drop: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            frames,
            on_drop: Some(Box::new(on_drop)),
        }
    }
}
impl Drop for FrameSubscription {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

#[async_trait]
pub trait FrameSource: Send + Sync {
    async fn subscribe(&self, profile: EffectiveProfile) -> Result<FrameSubscription, ViewError>;
    async fn request_refresh(&self, action_seq: u64) -> Result<(), ViewError>;
    async fn set_profile(&self, profile: EffectiveProfile) -> Result<(), ViewError>;
    async fn stop(&self) -> Result<(), ViewError>;
}
/// Where human input is applied.
///
/// # Contract: cancel-safe and idempotent
///
/// The daemon bounds every call with a timeout and then DROPS the future (and
/// aborts the task it runs in). Implementations must therefore be cancel-safe:
/// dropping a call at any `.await` leaves no half-applied state that a later
/// call cannot repair, and must not detach work that keeps running after the
/// drop (for example `spawn_blocking` or `tokio::spawn` whose result nobody
/// awaits). [`InputSink::release_pressed_inputs`] must be idempotent: it is
/// retried until it is confirmed, also after a restart, and releasing keys that
/// are not pressed is a no-op. `inject` may be cancelled between events but
/// never retried by the daemon after a partial application.
///
/// # Hard rule: `inject` must not outlive `release_pressed_inputs`
///
/// After the daemon has aborted an `inject` (timeout, cancelled request) it
/// calls `release_pressed_inputs`, possibly repeatedly until it is confirmed
/// (also after a failed or partial batch, a disconnect, or a restart). No
/// keystroke or click of an aborted `inject` may be delivered after
/// `release_pressed_inputs` has returned. A backend that applies input on
/// another thread (`spawn_blocking`, an OS hook thread) must therefore hold the
/// "keys released" barrier itself: `release_pressed_inputs` must wait for, or
/// cancel, that thread's queued work before it reports success. Synchronous
/// code that blocks without an `.await` cannot be aborted by the daemon at all
/// and is not allowed in `inject`.
#[async_trait]
pub trait InputSink: Send + Sync {
    /// One event, called by the daemon's serialized ownership executor only.
    async fn inject(&self, event: &InputEvent) -> Result<(), ViewError>;
    /// Inject one event validated against the `(generation, geometry_revision)`
    /// the client computed it from.
    ///
    /// Backends whose coordinate mapping depends on frame-synchronized display
    /// geometry override this to reject stale revisions fail-closed; targets
    /// whose input path already delegates to [`Self::inject`] (such as the
    /// browser target) inherit the default delegation unchanged.
    async fn inject_at_revision(
        &self,
        event: &InputEvent,
        _generation: u64,
        _geometry_revision: u64,
    ) -> Result<(), ViewError> {
        self.inject(event).await
    }
    async fn release_pressed_inputs(&self) -> Result<(), ViewError>;
    /// Pin only real takeover; subscribing in watch must never launch or pin.
    async fn set_takeover_active(&self, _active: bool) -> Result<(), ViewError> {
        Ok(())
    }
}
#[async_trait]
pub trait ViewTarget: Send + Sync {
    fn info(&self) -> TargetInfo;
    fn frame_source(&self) -> Arc<dyn FrameSource>;
    fn input_sink(&self) -> Option<Arc<dyn InputSink>>;
}

type Hook = Arc<dyn Fn() + Send + Sync>;
type BotHook = Arc<dyn Fn(Uuid) + Send + Sync>;

/// Injection point for existing backend instances, never a browser manager.
pub struct TargetRegistry {
    targets: RwLock<BTreeMap<Uuid, Arc<dyn ViewTarget>>>,
    unavailable: RwLock<Vec<TargetUnavailable>>,
    /// Pixel-free "the set of targets or one target's state changed" signal.
    changes: broadcast::Sender<()>,
    /// Asks the owner of a missing screen to look again (see [`Self::request_reprobe`]).
    reprobe: RwLock<Option<Hook>>,
    /// Runs when a bot is retired, so its owner can drop what it holds for it.
    bot_retired: RwLock<Vec<BotHook>>,
    /// Owners of terminal sessions (see [`BotTerminalSource`]).
    terminals: crate::terminal::BotTerminalSources,
}
impl Default for TargetRegistry {
    fn default() -> Self {
        Self {
            targets: RwLock::default(),
            unavailable: RwLock::default(),
            changes: broadcast::channel(16).0,
            reprobe: RwLock::default(),
            bot_retired: RwLock::default(),
            terminals: crate::terminal::BotTerminalSources::default(),
        }
    }
}
impl TargetRegistry {
    /// Target lifecycle notifications: a target was added, removed or changed
    /// state (idle, attached, closed), or a missing-screen note changed. Carries
    /// nothing; readers refetch `GET /v1/view/targets`. A lagging reader only
    /// loses a hint.
    #[must_use]
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }
    /// Signal a change (a target changed its own state). Cheap, never blocks.
    pub fn notify_changed(&self) {
        let _ = self.changes.send(());
    }
    /// Record that `kind` of screen cannot be offered (replaces an earlier note
    /// of the same kind and host; ignored while such a target is registered).
    /// Never exposes pixels, paths or free text.
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn note_unavailable(&self, note: TargetUnavailable) {
        let changed = {
            let targets = self.targets.read().expect("registry poisoned");
            // A kind that is offered has no reason to be missing: a late note
            // (a slow probe's timeout) must not outlive the registration.
            if targets.values().any(|t| {
                let info = t.info();
                info.kind == note.kind && info.host_id == note.host_id
            }) {
                return;
            }
            let mut all = self.unavailable.write().expect("registry poisoned");
            let same = all.contains(&note);
            all.retain(|n| !(n.kind == note.kind && n.host_id == note.host_id));
            all.push(note);
            !same
        };
        if changed {
            self.notify_changed();
        }
    }
    /// Why some kinds of screen are missing (empty when everything is offered).
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    #[must_use]
    pub fn unavailable(&self) -> Vec<TargetUnavailable> {
        self.unavailable.read().expect("registry poisoned").clone()
    }
    /// Offer the terminal sessions of a tool to the live view (held weakly).
    pub fn add_terminal_source(&self, source: std::sync::Weak<dyn crate::BotTerminalSource>) {
        self.terminals.add(source);
    }
    /// Every terminal session of every source, newest first. Text only, already masked.
    #[must_use]
    pub fn terminals(&self) -> Vec<crate::BotTerminal> {
        self.terminals.list()
    }
    /// Offer a target. Any "cannot offer this kind of screen" note for the same
    /// kind and host is cleared in the same step, so a late success never
    /// leaves a stale reason behind.
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn register(&self, target: Arc<dyn ViewTarget>) {
        let info = target.info();
        {
            let mut targets = self.targets.write().expect("registry poisoned");
            let mut notes = self.unavailable.write().expect("registry poisoned");
            notes.retain(|n| !(n.kind == info.kind && n.host_id == info.host_id));
            targets.insert(info.target_id, target);
        }
        self.notify_changed();
    }
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn remove(&self, id: Uuid) {
        let removed = self
            .targets
            .write()
            .expect("registry poisoned")
            .remove(&id)
            .is_some();
        if removed {
            self.notify_changed();
        }
    }
    /// Remove every target that belongs to `bot_id` and tell the owners that
    /// registered for it ([`Self::on_bot_retired`]) to drop their drivers.
    /// Returns how many targets were removed.
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn retire_bot(&self, bot_id: Uuid) -> usize {
        let removed = {
            let mut targets = self.targets.write().expect("registry poisoned");
            let ids: Vec<Uuid> = targets
                .iter()
                .filter(|(_, t)| t.info().bot_id == Some(bot_id))
                .map(|(id, _)| *id)
                .collect();
            for id in &ids {
                targets.remove(id);
            }
            ids.len()
        };
        let hooks: Vec<BotHook> = self.bot_retired.read().expect("registry poisoned").clone();
        for hook in hooks {
            hook(bot_id);
        }
        if removed > 0 {
            self.notify_changed();
        }
        removed
    }
    /// Run `hook` whenever a bot is retired (deleted).
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn on_bot_retired(&self, hook: impl Fn(Uuid) + Send + Sync + 'static) {
        self.bot_retired
            .write()
            .expect("registry poisoned")
            .push(Arc::new(hook));
    }
    /// Install the function that looks for a screen again after a transient
    /// failure. It must return at once (spawn, do not block) and rate-limit itself.
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn set_reprobe(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.reprobe.write().expect("registry poisoned") = Some(Arc::new(hook));
    }
    /// A viewer asked: look again for screens that were unavailable. Never blocks.
    ///
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn request_reprobe(&self) {
        let hook = self.reprobe.read().expect("registry poisoned").clone();
        if let Some(hook) = hook {
            hook();
        }
    }
    #[must_use]
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn get(&self, id: Uuid) -> Option<Arc<dyn ViewTarget>> {
        self.targets
            .read()
            .expect("registry poisoned")
            .get(&id)
            .cloned()
    }
    #[must_use]
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn targets(&self) -> Vec<TargetInfo> {
        self.targets
            .read()
            .expect("registry poisoned")
            .values()
            .map(|t| t.info())
            .collect()
    }
}

/// Hermetic, externally clocked synthetic source. No task, OS, file, or network.
type LatestSender = watch::Sender<Option<Arc<Frame>>>;
#[derive(Default)]
pub struct SyntheticSource {
    tx: Arc<std::sync::Mutex<Option<LatestSender>>>,
}
impl SyntheticSource {
    /// # Errors
    /// Returns a fixed protocol error for invalid, unsupported, or oversized data.
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn publish(&self, frame: Frame) -> Result<(), ViewError> {
        frame.validate()?;
        if let Some(tx) = self.tx.lock().expect("source poisoned").as_ref() {
            // No receiver: do not keep the last pixel buffer alive.
            if tx.receiver_count() > 0 {
                tx.send_replace(Some(Arc::new(frame)));
            }
        }
        Ok(())
    }
    #[must_use]
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn subscribers(&self) -> usize {
        self.tx
            .lock()
            .expect("source poisoned")
            .as_ref()
            .map_or(0, watch::Sender::receiver_count)
    }
}
#[async_trait]
impl FrameSource for SyntheticSource {
    async fn subscribe(&self, _profile: EffectiveProfile) -> Result<FrameSubscription, ViewError> {
        let mut tx = self.tx.lock().expect("source poisoned");
        let sender = tx.get_or_insert_with(|| watch::channel(None).0);
        if sender.receiver_count() == 0 {
            sender.send_replace(None);
        }
        let rx = sender.subscribe();
        let shared = Arc::clone(&self.tx);
        Ok(FrameSubscription::new(rx, move || {
            if let Some(tx) = shared.lock().expect("source poisoned").as_ref()
                && tx.receiver_count() <= 1
            {
                tx.send_replace(None);
            }
        }))
    }
    async fn request_refresh(&self, _action_seq: u64) -> Result<(), ViewError> {
        Ok(())
    }
    async fn set_profile(&self, _profile: EffectiveProfile) -> Result<(), ViewError> {
        Ok(())
    }
    async fn stop(&self) -> Result<(), ViewError> {
        if let Some(tx) = self.tx.lock().expect("source poisoned").as_ref() {
            tx.send_replace(None);
        }
        Ok(())
    }
}
