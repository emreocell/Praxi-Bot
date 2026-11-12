//! Object-safe backend hooks. Dropping the last subscription ends capture.
use crate::{EffectiveProfile, FrameMetadata, InputEvent, MAX_PAYLOAD, TargetInfo, ViewError};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;
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
            || m.width > 1920
            || m.height > 1080
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
#[async_trait]
pub trait InputSink: Send + Sync {
    /// One event, called by the daemon's serialized ownership executor only.
    async fn inject(&self, event: &InputEvent) -> Result<(), ViewError>;
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

/// Injection point for existing backend instances, never a browser manager.
#[derive(Default)]
pub struct TargetRegistry(RwLock<BTreeMap<Uuid, Arc<dyn ViewTarget>>>);
impl TargetRegistry {
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn register(&self, target: Arc<dyn ViewTarget>) {
        self.0
            .write()
            .expect("registry poisoned")
            .insert(target.info().target_id, target);
    }
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn remove(&self, id: Uuid) {
        self.0.write().expect("registry poisoned").remove(&id);
    }
    #[must_use]
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn get(&self, id: Uuid) -> Option<Arc<dyn ViewTarget>> {
        self.0.read().expect("registry poisoned").get(&id).cloned()
    }
    #[must_use]
    /// # Panics
    /// Panics only if an internal synchronization lock was poisoned.
    pub fn targets(&self) -> Vec<TargetInfo> {
        self.0
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
