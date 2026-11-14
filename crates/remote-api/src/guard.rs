//! Lifetime of one long-lived response body.
//!
//! The wrapper never copies, collects or reorders data: it forwards each frame
//! unchanged and only adds three things. The permits and driver of the exchange
//! live exactly as long as the body. A stalled consumer (a frame was handed out
//! and the next one was not requested for `stall`) cancels the stream. And a
//! cancelled stream ends in an *error*, never a clean end, so a client cannot
//! mistake a cut record for a finished response.
use crate::AbortTask;
use axum::body::{Body, Bytes};
use hyper::body::{Body as HttpBody, Frame};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

const IDLE: u64 = u64::MAX;

/// Resources and cancellation shared by a body and its watchdog.
pub(crate) struct Lease {
    cancel: CancellationToken,
    held: Mutex<Option<Box<dyn Send>>>,
    /// Milliseconds since `base` at which a frame was handed to the consumer
    /// and not yet followed by a poll, or [`IDLE`].
    pending_since: AtomicU64,
    base: Instant,
    stalled: std::sync::atomic::AtomicBool,
}

impl Lease {
    pub(crate) fn new(cancel: CancellationToken, held: impl Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            cancel,
            held: Mutex::new(Some(Box::new(held))),
            pending_since: AtomicU64::new(IDLE),
            base: Instant::now(),
            stalled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Return permits and drop the exchange driver (resetting its QUIC stream).
    pub(crate) fn release(&self) {
        let held = self.held.lock().ok().and_then(|mut g| g.take());
        drop(held);
    }

    fn mark_pending(&self) {
        let ms = u64::try_from(self.base.elapsed().as_millis()).unwrap_or(IDLE - 1);
        self.pending_since.store(ms, Ordering::Release);
    }

    fn mark_polled(&self) {
        self.pending_since.store(IDLE, Ordering::Release);
    }

    fn stall(&self) {
        self.stalled.store(true, Ordering::Release);
        self.cancel.cancel();
        self.release();
    }

    async fn watch(self: Arc<Self>, stall: Duration) {
        let tick = (stall / 4).clamp(Duration::from_millis(20), Duration::from_secs(2));
        loop {
            let since = self.pending_since.load(Ordering::Acquire);
            if since == IDLE {
                tokio::time::sleep(tick).await;
                continue;
            }
            let due = Duration::from_millis(since) + stall;
            let now = self.base.elapsed();
            if now >= due {
                // Re-check: the consumer may have polled between the two loads.
                if self.pending_since.load(Ordering::Acquire) == since {
                    self.stall();
                    return;
                }
            } else {
                tokio::time::sleep(due.saturating_sub(now).min(tick)).await;
            }
        }
    }
}

pub(crate) struct GuardedBody {
    inner: Body,
    lease: Arc<Lease>,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    _watchdog: Option<AbortTask>,
    finished: bool,
}

impl GuardedBody {
    /// `stall` of `None` disables the consumer-stall watchdog (SSE, ordinary).
    pub(crate) fn new(inner: Body, lease: Arc<Lease>, stall: Option<Duration>) -> Self {
        let watchdog = stall.map(|stall| AbortTask(tokio::spawn(Arc::clone(&lease).watch(stall))));
        Self {
            inner,
            cancelled: Box::pin(lease.cancel.clone().cancelled_owned()),
            lease,
            _watchdog: watchdog,
            finished: false,
        }
    }
}

impl Drop for GuardedBody {
    fn drop(&mut self) {
        // Permits and driver follow the body. The cancel token is *not* fired:
        // a body that completed normally must not cut the final flush.
        self.lease.release();
    }
}

impl HttpBody for GuardedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = &mut *self;
        if this.finished {
            return Poll::Ready(None);
        }
        if this.cancelled.as_mut().poll(cx).is_ready() {
            this.finished = true;
            this.lease.release();
            let reason = if this.lease.stalled.load(Ordering::Acquire) {
                "stream consumer stalled"
            } else {
                "stream cancelled"
            };
            return Poll::Ready(Some(Err(axum::Error::new(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                reason,
            )))));
        }
        this.lease.mark_polled();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if frame.is_data() {
                    this.lease.mark_pending();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.finished = true;
                this.lease.release();
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                this.finished = true;
                this.lease.release();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
