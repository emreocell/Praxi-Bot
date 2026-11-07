//! `GET /v1/events`: server-sent events of [`protocol::Envelope`]s.
//!
//! Two forms, both resumable with `Last-Event-ID`:
//!
//! * `?run_id=R` follows one run.  The SSE `id` is the run-local `seq`; with
//!   `Last-Event-ID: N` the events `> N` are replayed from the run's NDJSON log
//!   (everything when absent) and the stream then goes live, without gaps or
//!   duplicates.  The stream ends after the run's `run_finished`.
//! * No `run_id`: the daemon-wide stream of all runs.  Every event carries a
//!   daemon-wide, strictly increasing `gseq` (persisted in the storage
//!   `event_index` table); the SSE `id` is that `gseq`.  With
//!   `Last-Event-ID: G` every event `> G` of every run is replayed in order
//!   (index rows -> the runs' NDJSON logs), then the stream goes live.  Without
//!   it the stream is live only.  A subscriber that lags behind the broadcast
//!   buffer recovers the missed events from the index; nothing is skipped.
//!
//! A browser `EventSource` cannot set headers, so this route (and only this
//! route) also accepts the API token as `?token=...` (see `auth`).

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::StreamItem;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::stream::{self, Stream};
use protocol::{Envelope, Event, RunId};
use serde::Deserialize;
use storage::{Storage, read_event_log};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::AppState;
use crate::dto::ScheduleChanged;
use crate::error::ApiError;
use crate::mcp::McpServerChanged;

/// Index rows replayed per batch (bounds memory of a long replay).
const REPLAY_CHUNK: u32 = 500;

/// How long the global replay waits for an index row whose log line is not on
/// disk yet (the writer indexes first, then appends) before it treats the line
/// as lost to a crash and moves on.
const PENDING_LINE_GRACE: Duration = Duration::from_secs(3);

/// Pause between re-reads while waiting for such a line.
const PENDING_RETRY: Duration = Duration::from_millis(50);

/// Attempts to read a run's log before the stream gives up and closes (the
/// client then reconnects with its cursor).
const LOG_READ_ATTEMPTS: u32 = 3;

/// Query of `GET /v1/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Follow only this run.
    pub run_id: Option<Uuid>,
    /// API token for clients that cannot send an `Authorization` header
    /// (consumed by the auth guard; ignored here).
    #[serde(default)]
    #[allow(dead_code)]
    pub token: Option<String>,
}

/// Where a subscriber resumes.
#[derive(Debug, PartialEq, Eq)]
enum Cursor {
    /// One run, after this `seq` (`None` = from the start).
    Run(RunId, Option<u64>),
    /// All runs, after this `gseq` (`None` = live only).
    Global(Option<u64>),
}

fn parse_cursor(scoped_run: Option<RunId>, header: Option<&str>) -> Result<Cursor, ApiError> {
    let bad = || ApiError::bad_request("malformed Last-Event-ID");
    let last = match header.map(str::trim).filter(|h| !h.is_empty()) {
        None => None,
        Some(h) => Some(h.parse::<u64>().map_err(|_| bad())?),
    };
    Ok(match scoped_run {
        Some(run) => Cursor::Run(run, last),
        None => Cursor::Global(last),
    })
}

#[allow(clippy::struct_excessive_bools)]
struct Feed {
    rx: broadcast::Receiver<StreamItem>,
    backlog: VecDeque<StreamItem>,
    scoped: bool,
    // ---- per-run form ----
    run: Option<RunId>,
    /// Last delivered `seq` of the run.
    last: Option<u64>,
    done: bool,
    /// The run's log already holds its `run_finished`: end once drained.
    finished_in_log: bool,
    // ---- global form ----
    /// Highest `gseq` delivered or scanned (replayed / skipped as a duplicate).
    scan: u64,
    /// More index rows may be pending (replay in progress).
    replaying: bool,
    /// Since when the replay waits for an index row whose log line is missing.
    stalled_since: Option<Instant>,
    // ---- global form ----
    /// `schedule_changed` notifications (global stream only).
    sched_rx: Option<broadcast::Receiver<ScheduleChanged>>,
    /// `mcp_server_changed` notifications (global stream only).
    mcp_rx: Option<broadcast::Receiver<McpServerChanged>>,
    // ---- both ----
    storage: Arc<Storage>,
    data_dir: PathBuf,
    cancel: CancellationToken,
    /// Marks a global stream (the desktop app's) as open for the control bridge.
    _app: Option<crate::control::SubscriberGuard>,
}

impl Feed {
    /// Per-run form: reload the run's log after the delivered `seq`.  Returns
    /// `false` when the log stays unreadable (the caller ends the stream so the
    /// client resumes from its cursor instead of silently missing events).
    async fn reload_run(&mut self) -> bool {
        let Some(run) = self.run else { return true };
        let after = self.last;
        let mut logged = None;
        for attempt in 0..LOG_READ_ATTEMPTS {
            let dir = self.data_dir.clone();
            match tokio::task::spawn_blocking(move || read_event_log(&dir, run)).await {
                Ok(Ok(l)) => {
                    logged = Some(l);
                    break;
                }
                _ if attempt + 1 < LOG_READ_ATTEMPTS => tokio::time::sleep(PENDING_RETRY).await,
                _ => {}
            }
        }
        let Some(logged) = logged else { return false };
        self.finished_in_log |= logged
            .iter()
            .any(|e| matches!(e.event, Event::RunFinished(_)));
        self.backlog = logged
            .into_iter()
            .filter(|e| after.is_none_or(|a| e.seq > a))
            .map(|envelope| StreamItem {
                gseq: 0,
                run_id: run,
                envelope,
            })
            .collect();
        true
    }

    /// Global form: load the next chunk of index rows after `scan`.
    async fn replay_chunk(&mut self) {
        let (st, dir, after) = (Arc::clone(&self.storage), self.data_dir.clone(), self.scan);
        let force = self
            .stalled_since
            .is_some_and(|t| t.elapsed() >= PENDING_LINE_GRACE);
        let loaded =
            tokio::task::spawn_blocking(move || load_global(&st, &dir, after, force)).await;
        if let Ok(Some(chunk)) = loaded {
            self.scan = self.scan.max(chunk.max_gseq);
            self.replaying = chunk.more;
            if chunk.stalled {
                self.stalled_since.get_or_insert_with(Instant::now);
            } else {
                self.stalled_since = None;
            }
            self.backlog.extend(chunk.items);
        } else {
            // Storage trouble: carry on live rather than stall the stream.
            self.replaying = false;
            self.stalled_since = None;
        }
    }

    /// Next item to deliver, `None` when the stream is over.
    async fn next_item(&mut self) -> Option<Out> {
        if self.scoped {
            self.next_run_item().await.map(Out::Run)
        } else {
            self.next_global_item().await
        }
    }

    async fn next_run_item(&mut self) -> Option<StreamItem> {
        loop {
            if self.done {
                return None;
            }
            if let Some(item) = self.backlog.pop_front() {
                if self.accept_run(&item) {
                    return Some(item);
                }
                continue;
            }
            if self.finished_in_log {
                return None;
            }
            let received = tokio::select! {
                () = self.cancel.cancelled() => return None,
                r = self.rx.recv() => r,
            };
            match received {
                Ok(item) => {
                    if Some(item.run_id) != self.run {
                        continue;
                    }
                    if self.accept_run(&item) {
                        return Some(item);
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if !self.reload_run().await {
                        return None;
                    }
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// De-duplicate against the replayed run and update the cursor.
    fn accept_run(&mut self, item: &StreamItem) -> bool {
        let seq = item.envelope.seq;
        if self.last.is_some_and(|l| seq <= l) {
            return false;
        }
        self.last = Some(seq);
        if matches!(item.envelope.event, Event::RunFinished(_)) {
            self.done = true;
        }
        true
    }

    async fn next_global_item(&mut self) -> Option<Out> {
        loop {
            if self.cancel.is_cancelled() {
                return None;
            }
            if let Some(item) = self.backlog.pop_front() {
                return Some(Out::Run(item));
            }
            if self.replaying {
                self.replay_chunk().await;
                continue;
            }
            if self.stalled_since.is_some() {
                // The next index row's log line is not on disk yet.  Delivering
                // a later live event would move the cursor past it for good, so
                // wait (live events are re-read from the index) and retry.
                tokio::select! {
                    () = self.cancel.cancelled() => return None,
                    () = tokio::time::sleep(PENDING_RETRY) => {}
                    r = self.rx.recv() => {
                        if matches!(r, Err(RecvError::Closed)) {
                            return None;
                        }
                    }
                }
                self.replaying = true;
                continue;
            }
            let (rx, sched_rx, mcp_rx) = (&mut self.rx, &mut self.sched_rx, &mut self.mcp_rx);
            let received = tokio::select! {
                () = self.cancel.cancelled() => return None,
                r = rx.recv() => r,
                s = async {
                    match sched_rx {
                        Some(r) => r.recv().await,
                        None => std::future::pending().await,
                    }
                } => match s {
                    Ok(change) => return Some(Out::Schedule(change)),
                    // A dropped notification only means a missed refetch hint.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => {
                        self.sched_rx = None;
                        continue;
                    }
                },
                m = async {
                    match mcp_rx {
                        Some(r) => r.recv().await,
                        None => std::future::pending().await,
                    }
                } => match m {
                    Ok(change) => return Some(Out::Mcp(change)),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => {
                        self.mcp_rx = None;
                        continue;
                    }
                },
            };
            match received {
                Ok(item) => {
                    // Already replayed (or older than the cursor).
                    if item.gseq <= self.scan {
                        continue;
                    }
                    self.scan = item.gseq;
                    return Some(Out::Run(item));
                }
                // Recover what the buffer dropped from the index.
                Err(RecvError::Lagged(_)) => self.replaying = true,
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

/// What the stream delivers next.
enum Out {
    Run(StreamItem),
    Schedule(ScheduleChanged),
    Mcp(McpServerChanged),
}

/// One batch of the global replay.
struct Chunk {
    items: Vec<StreamItem>,
    /// Highest index `gseq` examined (also when its log line is missing).
    max_gseq: u64,
    /// The batch was full: more rows may follow.
    more: bool,
    /// The batch stopped before an index row whose log line may still be
    /// written (or whose log could not be read): retry from `max_gseq`.
    stalled: bool,
}

/// One run's log as the replay sees it (`None`: it could not be read).
type RunLog = Option<HashMap<u64, Envelope>>;

/// Index rows `> after` joined with the runs' NDJSON logs.
///
/// The writer indexes a row *before* it appends the log line, and this reader
/// takes no lock, so a missing line is usually just "not written yet".  The
/// batch then stops *before* that row (the cursor must not pass it) and sets
/// `stalled`.  A row is skipped only when its loss is certain: a later line of
/// the same run is on disk (lines of a run are appended in order), or `force`
/// is set because the grace period for a crash gap ran out.  An unreadable log
/// stalls the same way.
fn load_global(st: &Storage, dir: &std::path::Path, after: u64, force: bool) -> Option<Chunk> {
    let rows = st.list_event_index(after, REPLAY_CHUNK).ok()?;
    let more = rows.len() >= REPLAY_CHUNK as usize;
    let mut logs: HashMap<RunId, RunLog> = HashMap::new();
    let mut items = Vec::with_capacity(rows.len());
    let mut max_gseq = after;
    let mut stalled = false;
    for row in rows {
        let log = logs.entry(row.run_id).or_insert_with(|| {
            read_event_log(dir, row.run_id)
                .ok()
                .map(|l| l.into_iter().map(|e| (e.seq, e)).collect())
        });
        match log {
            Some(m) => {
                if let Some(env) = m.get(&row.seq) {
                    items.push(StreamItem {
                        gseq: row.gseq,
                        run_id: row.run_id,
                        envelope: env.clone(),
                    });
                } else if !force && !m.keys().any(|s| *s > row.seq) {
                    stalled = true;
                    break;
                }
            }
            None if force => {}
            None => {
                stalled = true;
                break;
            }
        }
        max_gseq = max_gseq.max(row.gseq);
    }
    Some(Chunk {
        items,
        max_gseq,
        // Stopped early: the rest is re-read from the cursor after the pause.
        more: more && !stalled,
        stalled,
    })
}

fn to_sse(item: &StreamItem, scoped: bool) -> SseEvent {
    let env: &Envelope = &item.envelope;
    let id = if scoped {
        env.seq.to_string()
    } else {
        item.gseq.to_string()
    };
    let name = serde_json::to_value(&env.event)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "event".to_owned());
    SseEvent::default()
        .id(id)
        .event(name)
        .json_data(env)
        .unwrap_or_else(|_| SseEvent::default().comment("unserialisable event"))
}

fn event_stream(feed: Feed) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    stream::unfold(feed, |mut feed| async move {
        let ev = match feed.next_item().await? {
            Out::Run(item) => to_sse(&item, feed.scoped),
            Out::Schedule(change) => SseEvent::default()
                .event("schedule_changed")
                .json_data(&change)
                .unwrap_or_else(|_| SseEvent::default().comment("unserialisable event")),
            Out::Mcp(change) => SseEvent::default()
                .event("mcp_server_changed")
                .json_data(&change)
                .unwrap_or_else(|_| SseEvent::default().comment("unserialisable event")),
        };
        Some((Ok(ev), feed))
    })
}

/// `GET /v1/events`.
pub(crate) async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let scoped_run = q.run_id.map(RunId);
    if let Some(run) = scoped_run {
        state.run_of_user(run)?;
    }
    let last_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cursor = parse_cursor(scoped_run, last_id.as_deref())?;
    // Subscribe before reading the log / index so nothing falls between the two.
    let rx = state.agent().subscribe();
    let mut feed = Feed {
        rx,
        backlog: VecDeque::new(),
        scoped: false,
        run: None,
        last: None,
        done: false,
        finished_in_log: false,
        scan: 0,
        replaying: false,
        stalled_since: None,
        sched_rx: scoped_run
            .is_none()
            .then(|| state.subscribe_schedule_events()),
        mcp_rx: scoped_run
            .is_none()
            .then(|| state.mcp().map(|m| m.subscribe()))
            .flatten(),
        storage: state.storage_arc(),
        data_dir: state.data_dir(),
        cancel: state.shutdown_token(),
        _app: scoped_run.is_none().then(|| state.hub().subscribe()),
    };
    match cursor {
        Cursor::Run(run, last) => {
            feed.scoped = true;
            feed.run = Some(run);
            feed.last = last;
            if !feed.reload_run().await {
                return Err(ApiError::internal("the run's event log is unreadable"));
            }
        }
        Cursor::Global(resume) => {
            let st = Arc::clone(&feed.storage);
            let newest = tokio::task::spawn_blocking(move || st.last_gseq())
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            match resume {
                // Replay everything after the cursor (a cursor from the future,
                // e.g. a reset database, is clamped).
                Some(g) => {
                    feed.scan = g.min(newest);
                    feed.replaying = true;
                }
                // Live only: events up to now are history.
                None => feed.scan = newest,
            }
        }
    }
    Ok(Sse::new(event_stream(feed)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_forms() {
        let run = RunId::new();
        assert_eq!(
            parse_cursor(Some(run), Some("7")).unwrap(),
            Cursor::Run(run, Some(7))
        );
        assert_eq!(
            parse_cursor(Some(run), None).unwrap(),
            Cursor::Run(run, None)
        );
        assert_eq!(
            parse_cursor(None, Some(" 12 ")).unwrap(),
            Cursor::Global(Some(12))
        );
        assert_eq!(parse_cursor(None, None).unwrap(), Cursor::Global(None));
        assert_eq!(parse_cursor(None, Some("")).unwrap(), Cursor::Global(None));
        // The pre-gseq `<run>:<seq>` form is gone.
        assert!(parse_cursor(None, Some(&format!("{run}:3"))).is_err());
        assert!(parse_cursor(Some(run), Some("x")).is_err());
        assert!(parse_cursor(None, Some("-1")).is_err());
    }

    #[test]
    fn global_replay_joins_the_index_with_the_run_logs() {
        use protocol::event::MessageDelta;
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let (a, b) = (RunId::new(), RunId::new());
        let env = |run: RunId, seq: u64| {
            Envelope::new(
                seq,
                Event::MessageDelta(MessageDelta {
                    run_id: run,
                    text: format!("{run}/{seq}"),
                }),
            )
        };
        let mut wa = storage::EventLogWriter::open(dir.path(), a).unwrap();
        let mut wb = storage::EventLogWriter::open(dir.path(), b).unwrap();
        // Interleaved: a0 b0 a1 b1(log line missing: crash) a2
        wa.append(&env(a, 0)).unwrap();
        wb.append(&env(b, 0)).unwrap();
        wa.append(&env(a, 1)).unwrap();
        wa.append(&env(a, 2)).unwrap();
        for (g, run, seq) in [(1, a, 0), (2, b, 0), (3, a, 1), (4, b, 1), (5, a, 2)] {
            st.index_event(g, run, seq).unwrap();
        }
        let chunk = load_global(&st, dir.path(), 0, false).unwrap();
        let got: Vec<(u64, RunId, u64)> = chunk
            .items
            .iter()
            .map(|i| (i.gseq, i.run_id, i.envelope.seq))
            .collect();
        // b1 may still be written: the batch stops before it, the cursor too.
        assert_eq!(got, [(1, a, 0), (2, b, 0), (3, a, 1)]);
        assert_eq!(chunk.max_gseq, 3, "the cursor must not pass a pending row");
        assert!(chunk.stalled && !chunk.more);
        // Once the grace period is over the line counts as lost to a crash.
        let forced = load_global(&st, dir.path(), 3, true).unwrap();
        assert_eq!(forced.items.len(), 1);
        assert_eq!(forced.items[0].gseq, 5);
        assert!(!forced.stalled);
        assert_eq!(forced.max_gseq, 5);
        let none = load_global(&st, dir.path(), 5, false).unwrap();
        assert!(none.items.is_empty() && none.max_gseq == 5 && !none.stalled);
    }

    fn msg(run: RunId, seq: u64) -> Envelope {
        use protocol::event::MessageDelta;
        Envelope::new(
            seq,
            Event::MessageDelta(MessageDelta {
                run_id: run,
                text: format!("{seq}"),
            }),
        )
    }

    #[test]
    fn replay_does_not_skip_a_row_whose_log_line_arrives_later() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let a = RunId::new();
        let mut w = storage::EventLogWriter::open(dir.path(), a).unwrap();
        w.append(&msg(a, 0)).unwrap();
        // The writer has indexed seq 1 but not appended it yet.
        st.index_event(1, a, 0).unwrap();
        st.index_event(2, a, 1).unwrap();
        let first = load_global(&st, dir.path(), 0, false).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!((first.max_gseq, first.stalled), (1, true));
        // The line lands; resuming from the stalled cursor delivers it.
        w.append(&msg(a, 1)).unwrap();
        let second = load_global(&st, dir.path(), first.max_gseq, false).unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].gseq, 2);
        assert!(!second.stalled);
    }

    #[test]
    fn replay_skips_a_row_that_is_provably_lost() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let a = RunId::new();
        let mut w = storage::EventLogWriter::open(dir.path(), a).unwrap();
        // seq 1 never reached the log (crash), seq 2 did.
        w.append(&msg(a, 0)).unwrap();
        w.append(&msg(a, 2)).unwrap();
        for (g, seq) in [(1, 0), (2, 1), (3, 2)] {
            st.index_event(g, a, seq).unwrap();
        }
        let chunk = load_global(&st, dir.path(), 0, false).unwrap();
        let got: Vec<u64> = chunk.items.iter().map(|i| i.gseq).collect();
        assert_eq!(got, [1, 3]);
        assert!(!chunk.stalled);
        assert_eq!(chunk.max_gseq, 3);
    }

    #[test]
    fn replay_stalls_on_an_unreadable_log_instead_of_dropping_it() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let a = RunId::new();
        // A malformed non-final line makes read_event_log fail.
        let path = storage::event_log_path(dir.path(), a);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json\n{\"also\": \"bad\"}\n").unwrap();
        st.index_event(1, a, 0).unwrap();
        let chunk = load_global(&st, dir.path(), 0, false).unwrap();
        assert!(chunk.items.is_empty());
        assert_eq!((chunk.max_gseq, chunk.stalled), (0, true));
    }
}
