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
use crate::error::ApiError;

/// Index rows replayed per batch (bounds memory of a long replay).
const REPLAY_CHUNK: u32 = 500;

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
    // ---- both ----
    storage: Arc<Storage>,
    data_dir: PathBuf,
    cancel: CancellationToken,
    /// Marks a global stream (the desktop app's) as open for the control bridge.
    _app: Option<crate::control::SubscriberGuard>,
}

impl Feed {
    /// Per-run form: reload the run's log after the delivered `seq`.
    async fn reload_run(&mut self) {
        let Some(run) = self.run else { return };
        let dir = self.data_dir.clone();
        let after = self.last;
        let logged = tokio::task::spawn_blocking(move || read_event_log(&dir, run))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
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
    }

    /// Global form: load the next chunk of index rows after `scan`.
    async fn replay_chunk(&mut self) {
        let (st, dir, after) = (Arc::clone(&self.storage), self.data_dir.clone(), self.scan);
        let loaded = tokio::task::spawn_blocking(move || load_global(&st, &dir, after)).await;
        match loaded {
            Ok(Some(chunk)) => {
                self.scan = self.scan.max(chunk.max_gseq);
                self.replaying = chunk.more;
                self.backlog.extend(chunk.items);
            }
            // Storage trouble: carry on live rather than stall the stream.
            _ => self.replaying = false,
        }
    }

    /// Next item to deliver, `None` when the stream is over.
    async fn next_item(&mut self) -> Option<StreamItem> {
        if self.scoped {
            self.next_run_item().await
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
                Err(RecvError::Lagged(_)) => self.reload_run().await,
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

    async fn next_global_item(&mut self) -> Option<StreamItem> {
        loop {
            if self.cancel.is_cancelled() {
                return None;
            }
            if let Some(item) = self.backlog.pop_front() {
                return Some(item);
            }
            if self.replaying {
                self.replay_chunk().await;
                continue;
            }
            let received = tokio::select! {
                () = self.cancel.cancelled() => return None,
                r = self.rx.recv() => r,
            };
            match received {
                Ok(item) => {
                    // Already replayed (or older than the cursor).
                    if item.gseq <= self.scan {
                        continue;
                    }
                    self.scan = item.gseq;
                    return Some(item);
                }
                // Recover what the buffer dropped from the index.
                Err(RecvError::Lagged(_)) => self.replaying = true,
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

/// One batch of the global replay.
struct Chunk {
    items: Vec<StreamItem>,
    /// Highest index `gseq` examined (also when its log line is missing).
    max_gseq: u64,
    /// The batch was full: more rows may follow.
    more: bool,
}

/// Index rows `> after` joined with the runs' NDJSON logs.  A row whose log
/// line is missing (crash between the index insert and the log append) is
/// skipped.
fn load_global(st: &Storage, dir: &std::path::Path, after: u64) -> Option<Chunk> {
    let rows = st.list_event_index(after, REPLAY_CHUNK).ok()?;
    let more = rows.len() >= REPLAY_CHUNK as usize;
    let mut logs: HashMap<RunId, HashMap<u64, Envelope>> = HashMap::new();
    let mut items = Vec::with_capacity(rows.len());
    let mut max_gseq = after;
    for row in rows {
        max_gseq = max_gseq.max(row.gseq);
        let log = logs.entry(row.run_id).or_insert_with(|| {
            read_event_log(dir, row.run_id)
                .unwrap_or_default()
                .into_iter()
                .map(|e| (e.seq, e))
                .collect()
        });
        if let Some(env) = log.get(&row.seq) {
            items.push(StreamItem {
                gseq: row.gseq,
                run_id: row.run_id,
                envelope: env.clone(),
            });
        }
    }
    Some(Chunk {
        items,
        max_gseq,
        more,
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
        let item = feed.next_item().await?;
        let ev = to_sse(&item, feed.scoped);
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
            feed.reload_run().await;
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
        let chunk = load_global(&st, dir.path(), 0).unwrap();
        let got: Vec<(u64, RunId, u64)> = chunk
            .items
            .iter()
            .map(|i| (i.gseq, i.run_id, i.envelope.seq))
            .collect();
        assert_eq!(got, [(1, a, 0), (2, b, 0), (3, a, 1), (5, a, 2)]);
        assert_eq!(chunk.max_gseq, 5, "skipped rows still advance the cursor");
        assert!(!chunk.more);
        let rest = load_global(&st, dir.path(), 3).unwrap();
        assert_eq!(rest.items.len(), 1);
        assert_eq!(rest.items[0].gseq, 5);
        let none = load_global(&st, dir.path(), 5).unwrap();
        assert!(none.items.is_empty() && none.max_gseq == 5);
    }
}
