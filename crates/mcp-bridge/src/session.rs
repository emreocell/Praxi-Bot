//! Keep dependency diagnostics and their spawned tasks off application sinks.
//!
//! A tracing dispatcher attached to a future does not propagate through raw
//! `tokio::spawn` calls in rmcp's service and HTTP worker. Each session therefore
//! owns a current-thread runtime whose threads use [`Forwarder`]: it drops
//! everything below `warn` (protocol payloads are logged at lower levels) and
//! forwards rmcp's `warn`/`error` events (transport closed, parse failures, HTTP
//! status) to the caller's dispatcher only after the session's redactor and the
//! host's live redactor masked the message and its fields. Protocol messages and
//! tool results are not rewritten.
//!
//! Cost: one OS thread and one single-threaded runtime per session; both end
//! when the session closes or is dropped.

use std::{cell::RefCell, fmt, future::Future, time::Duration};

use parking_lot::Mutex;
use rmcp::{
    RoleClient,
    service::{Peer, RunningService},
};
use tokio::sync::{oneshot, watch};
use tracing::{Dispatch, Level, Metadata, subscriber::DefaultGuard};

use crate::{McpError, Result, diagnostics::Redactor};

const CLOSE_TIMEOUT: Duration = Duration::from_secs(8);
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

thread_local! {
    // Runtime callbacks also run on Tokio's blocking-pool threads. A guard must
    // live for the whole thread, not just for the on_thread_start callback.
    static QUIET_THREAD: RefCell<Option<DefaultGuard>> = const { RefCell::new(None) };
}

/// Collects an event's message and fields into one line.
#[derive(Default)]
struct LineVisitor(String);

impl tracing::field::Visit for LineVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        use fmt::Write as _;
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }
}

struct Forwarded {
    sink: Dispatch,
    level: Level,
    server: String,
    line: String,
}

/// Process-wide relay thread (started on first use) that emits [`Forwarded`]
/// events into the caller's dispatcher.
fn relay() -> &'static std::sync::mpsc::SyncSender<Forwarded> {
    static RELAY: std::sync::OnceLock<std::sync::mpsc::SyncSender<Forwarded>> =
        std::sync::OnceLock::new();
    RELAY.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Forwarded>(256);
        let spawned = std::thread::Builder::new()
            .name("mcp-diagnostics".to_owned())
            .spawn(move || {
                for Forwarded {
                    sink,
                    level,
                    server,
                    line,
                } in rx
                {
                    let server = server.as_str();
                    tracing::dispatcher::with_default(&sink, || {
                        if level == Level::ERROR {
                            tracing::error!(target: "mcp_bridge::rmcp", server, "{line}");
                        } else {
                            tracing::warn!(target: "mcp_bridge::rmcp", server, "{line}");
                        }
                    });
                }
            });
        drop(spawned);
        tx
    })
}

/// Subscriber installed on every session thread: forwards redacted
/// `warn`/`error` events and nothing else.
struct Forwarder {
    server: String,
    redactor: Redactor,
    sink: Dispatch,
    next_span: std::sync::atomic::AtomicU64,
}

impl tracing::Subscriber for Forwarder {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::WARN)
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let id = self
            .next_span
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::span::Id::from_u64(id.max(1))
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = LineVisitor::default();
        event.record(&mut line);
        let line = self.redactor.scrub(&line.0);
        // Emitting from inside a dispatch would nest dispatcher state, so a
        // single bounded relay thread does it; a flood is dropped, not queued.
        let _ = relay().try_send(Forwarded {
            sink: self.sink.clone(),
            level: *event.metadata().level(),
            server: self.server.clone(),
            line,
        });
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

// Runtime's ordinary Drop waits indefinitely for blocking work. Keep the same
// bounded shutdown on both normal return and unwinding of initialization.
struct BoundedRuntime(Option<tokio::runtime::Runtime>);

impl Drop for BoundedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        }
    }
}

/// The caller owns a peer and a lifetime lease; the runtime owns `RunningService`.
/// Dropping the final `Session` closes the lease even if explicit close is skipped.
pub(crate) struct Session {
    peer: Peer<RoleClient>,
    close: Mutex<Option<oneshot::Sender<()>>>,
    completed: watch::Receiver<bool>,
    pub(crate) redactor: Redactor,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn peer(&self) -> &Peer<RoleClient> {
        &self.peer
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.close.lock().is_none()
            || *self.completed.borrow()
            || self.completed.has_changed().is_err()
    }

    pub(crate) fn request_close(&self) {
        if let Some(close) = self.close.lock().take() {
            let _ = close.send(());
        }
    }

    /// Wait for service cleanup and private-runtime shutdown. Concurrent close
    /// callers all observe the same completion; no exclusive Arc is required.
    pub(crate) async fn close(&self) {
        self.request_close();
        let mut completed = self.completed.clone();
        let _ = completed.wait_for(|done| *done).await;
    }

    /// In-memory manager fixtures keep their service on the paused test clock.
    #[cfg(test)]
    pub(crate) fn from_running(inner: RunningService<RoleClient, ()>, redactor: Redactor) -> Self {
        let peer = inner.peer().clone();
        let (close, closing) = oneshot::channel();
        let (completed, completion) = watch::channel(false);
        tokio::spawn(async move {
            drive(inner, closing).await;
            let _ = completed.send(true);
        });
        Self {
            peer,
            close: Mutex::new(Some(close)),
            completed: completion,
            redactor,
        }
    }
}

/// Build and initialize a transport on its private runtime. The factory is
/// invoked there, so construction-time logs and HTTP workers are isolated too.
pub(crate) async fn open<F, Fut>(
    server: String,
    redactor: Redactor,
    connect_timeout: Duration,
    handshake: F,
) -> Result<Session>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<RunningService<RoleClient, ()>>>,
{
    // Keep this sender across the readiness await. Cancelling open (including
    // retirement or its caller's timeout) immediately interrupts the handshake.
    let (close, closing) = oneshot::channel();
    let (ready, readiness) = oneshot::channel();
    let (completed, completion) = watch::channel(false);
    let worker_server = server.clone();
    let quiet = Dispatch::new(Forwarder {
        server: server.clone(),
        redactor: redactor.clone(),
        sink: tracing::dispatcher::get_default(Clone::clone),
        next_span: std::sync::atomic::AtomicU64::new(1),
    });
    std::thread::Builder::new()
        .name("mcp-session".to_owned())
        .spawn(move || {
            let _quiet = tracing::dispatcher::set_default(&quiet);
            let thread_quiet = quiet.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .on_thread_start(move || {
                    QUIET_THREAD.with(|guard| {
                        *guard.borrow_mut() = Some(tracing::dispatcher::set_default(&thread_quiet));
                    });
                })
                .on_thread_stop(|| {
                    QUIET_THREAD.with(|guard| {
                        guard.borrow_mut().take();
                    });
                })
                .build();
            match runtime {
                Ok(runtime) => {
                    let runtime = BoundedRuntime(Some(runtime));
                    runtime
                        .0
                        .as_ref()
                        .expect("runtime is present until shutdown")
                        .block_on(initialize(
                            worker_server,
                            connect_timeout,
                            handshake,
                            closing,
                            ready,
                        ));
                    // Service shutdown is awaited before aborting any remaining
                    // tasks. Blocking DNS work cannot hold this thread forever;
                    // its pool threads retain their private quiet dispatcher.
                    drop(runtime);
                }
                Err(error) => {
                    let _ = ready.send(Err(McpError::Transport {
                        server: worker_server,
                        message: format!("cannot create MCP session runtime: {error}"),
                    }));
                }
            }
            // Explicit close observes completion only after runtime shutdown.
            let _ = completed.send(true);
        })
        .map_err(|error| McpError::Transport {
            server: server.clone(),
            message: format!("cannot start MCP session thread: {error}"),
        })?;
    let peer = readiness
        .await
        .map_err(|_| McpError::Transport {
            server,
            message: "MCP session ended before initialization completed".to_owned(),
        })?
        .map_err(|error| redactor.error(error))?;
    Ok(Session {
        peer,
        close: Mutex::new(Some(close)),
        completed: completion,
        redactor,
    })
}

async fn initialize<F, Fut>(
    server: String,
    timeout: Duration,
    handshake: F,
    mut closing: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<Peer<RoleClient>>>,
) where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<RunningService<RoleClient, ()>>>,
{
    let result = tokio::select! {
        biased;
        _ = &mut closing => return,
        // The async block makes factory invocation lazy: an already-closed
        // lease cannot spawn a child before the cancellation branch is polled.
        result = tokio::time::timeout(timeout, async move { handshake().await }) => result,
    };
    let inner = match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(error)) => {
            let _ = ready.send(Err(error));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err(McpError::Timeout {
                server,
                seconds: timeout.as_secs().max(1),
            }));
            return;
        }
    };
    // If open was cancelled just as the handshake finished, dropping its lease
    // makes drive cancel/close the service even though there is no receiver.
    let _ = ready.send(Ok(inner.peer().clone()));
    drive(inner, closing).await;
}

async fn drive(inner: RunningService<RoleClient, ()>, mut closing: oneshot::Receiver<()>) {
    let cancellation = inner.cancellation_token();
    let waiting = inner.waiting();
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        _ = &mut closing => {
            cancellation.cancel();
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, &mut waiting).await;
        }
        _ = &mut waiting => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use rmcp::{
        service::{RxJsonRpcMessage, TxJsonRpcMessage, serve_directly},
        transport::Transport,
    };
    use tracing::{Event, Metadata, Subscriber, span};

    use super::*;

    const PROBE_TARGET: &str = "mcp_bridge::session::private_dispatch_probe";

    struct ProbeSubscriber(Arc<AtomicUsize>);

    impl Subscriber for ProbeSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == PROBE_TARGET
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            if event.metadata().target() == PROBE_TARGET {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    fn emit_probe() {
        tracing::info!(target: PROBE_TARGET, "session-secret-sentinel");
    }

    struct TestTransport {
        close_started: Option<oneshot::Sender<()>>,
        release_close: Option<oneshot::Receiver<()>>,
        dropped: Option<oneshot::Sender<()>>,
        probe: bool,
    }

    impl Transport<RoleClient> for TestTransport {
        type Error = io::Error;

        fn send(
            &mut self,
            _: TxJsonRpcMessage<RoleClient>,
        ) -> impl Future<Output = io::Result<()>> + Send + 'static {
            std::future::ready(Ok(()))
        }

        async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
            std::future::pending().await
        }

        async fn close(&mut self) -> io::Result<()> {
            if self.probe {
                emit_probe();
            }
            if let Some(started) = self.close_started.take() {
                let _ = started.send(());
            }
            if let Some(release) = self.release_close.take() {
                let _ = release.await;
            }
            Ok(())
        }
    }

    impl Drop for TestTransport {
        fn drop(&mut self) {
            if self.probe {
                emit_probe();
            }
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[tokio::test]
    async fn private_dispatch_covers_raw_spawn_blocking_and_transport_cleanup() {
        // A process-wide trap is necessary: a task-local capture would itself
        // miss the raw spawned dependency tasks this regression protects.
        let observed = Arc::new(AtomicUsize::new(0));
        tracing::subscriber::set_global_default(ProbeSubscriber(Arc::clone(&observed))).unwrap();
        tokio::spawn(async { emit_probe() }).await.unwrap();
        assert_eq!(observed.load(Ordering::SeqCst), 1, "trap is not armed");

        let session = open(
            "quiet".to_owned(),
            Redactor::default(),
            Duration::from_secs(5),
            || async {
                emit_probe();
                tokio::spawn(async { emit_probe() }).await.unwrap();
                tokio::task::spawn_blocking(emit_probe).await.unwrap();
                Ok(serve_directly(
                    (),
                    TestTransport {
                        probe: true,
                        close_started: None,
                        release_close: None,
                        dropped: None,
                    },
                    None,
                ))
            },
        )
        .await
        .unwrap();
        let mut completed = session.completed.clone();
        drop(session);
        tokio::time::timeout(Duration::from_secs(2), completed.wait_for(|done| *done))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            observed.load(Ordering::SeqCst),
            1,
            "private task leaked to global tracing"
        );
    }

    /// Collects the message of every event it sees.
    #[derive(Clone, Default)]
    struct Collect(Arc<parking_lot::Mutex<Vec<(Level, String)>>>);

    impl tracing::Subscriber for Collect {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = LineVisitor::default();
            event.record(&mut line);
            self.0.lock().push((*event.metadata().level(), line.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn rmcp_warnings_and_errors_are_forwarded_redacted_and_lower_levels_dropped() {
        let sink = Collect::default();
        let _guard = tracing::subscriber::set_default(sink.clone());
        let secret = "rmcp-warn-sentinel-5d1e";
        let redactor = Redactor::new(
            [secret, "rmcp-warn-sentinel/url+value"],
            Some(Arc::new(|text: &str| {
                text.replace("host-live-secret", "<H>")
            })),
        );
        let session = open(
            "fwd".to_owned(),
            redactor,
            Duration::from_secs(5),
            || async {
                tracing::debug!("payload {}", "rmcp-warn-sentinel-5d1e");
                tracing::info!("payload {}", "rmcp-warn-sentinel-5d1e");
                tracing::warn!(
                    "transport closed: {} host-live-secret",
                    "rmcp-warn-sentinel-5d1e"
                );
                tokio::spawn(async {
                    tracing::error!(status = "rmcp-warn-sentinel/url+value", "http status 500");
                })
                .await
                .unwrap();
                Ok(serve_directly(
                    (),
                    TestTransport {
                        probe: false,
                        close_started: None,
                        release_close: None,
                        dropped: None,
                    },
                    None,
                ))
            },
        )
        .await
        .unwrap();
        session.close().await;
        let mut seen = sink.0.lock().clone();
        for _ in 0..200 {
            if seen.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            seen = sink.0.lock().clone();
        }
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].0, Level::WARN);
        assert!(
            seen[0].1.contains("transport closed: [REDACTED] <H>"),
            "{seen:?}"
        );
        assert_eq!(seen[1].0, Level::ERROR);
        assert!(seen[1].1.contains("http status 500"));
        assert!(
            !seen.iter().any(|(_, l)| l.contains("sentinel")),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_close_waits_for_transport_close_and_drop() {
        let (started, closing) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (dropped, mut dropped_rx) = oneshot::channel();
        let session = Arc::new(
            open(
                "cleanup".to_owned(),
                Redactor::default(),
                Duration::from_secs(5),
                move || async move {
                    Ok(serve_directly(
                        (),
                        TestTransport {
                            close_started: Some(started),
                            release_close: Some(released),
                            dropped: Some(dropped),
                            probe: false,
                        },
                        None,
                    ))
                },
            )
            .await
            .unwrap(),
        );
        let first = Arc::clone(&session);
        let first_close = tokio::spawn(async move { first.close().await });
        let second = Arc::clone(&session);
        let second_close = tokio::spawn(async move { second.close().await });
        closing.await.unwrap();
        assert!(session.is_closed());
        assert!(!first_close.is_finished());
        assert!(!second_close.is_finished());
        release.send(()).unwrap();
        first_close.await.unwrap();
        second_close.await.unwrap();
        dropped_rx
            .try_recv()
            .expect("close returned before transport drop");
    }

    struct DroppedHandshake(Option<oneshot::Sender<()>>);

    impl Drop for DroppedHandshake {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[tokio::test]
    async fn cancelling_open_drops_the_in_flight_handshake() {
        let (entered, entering) = oneshot::channel();
        let (dropped, dropping) = oneshot::channel();
        let opening = tokio::spawn(open(
            "cancel".to_owned(),
            Redactor::default(),
            Duration::from_secs(60),
            move || async move {
                let _guard = DroppedHandshake(Some(dropped));
                let _ = entered.send(());
                std::future::pending().await
            },
        ));
        entering.await.unwrap();
        opening.abort();
        assert!(opening.await.unwrap_err().is_cancelled());
        // Channel fencing proves cancellation without a wall-clock sleep. The
        // timeout is only a deadlock backstop for a regressed implementation.
        tokio::time::timeout(Duration::from_secs(2), dropping)
            .await
            .unwrap()
            .unwrap();
    }
}
