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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use agent_core::StreamItem;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::stream::{self, Stream};
use protocol::{Envelope, Event, RunId};
use serde::{Deserialize, Serialize};
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
    stalled_row: Option<u64>,
    failures: u32,
    retry_at: Option<tokio::time::Instant>,
    pending: VecDeque<Out>,
    live: BTreeMap<u64, StreamItem>,
    live_bytes: usize,
    live_closed: bool,
    /// Last event or explicit gap actually yielded, never merely scanned.
    delivered: u64,
    user_id: protocol::UserId,
    gaps_total: Arc<AtomicU64>,
    // ---- global form ----
    /// `schedule_changed` notifications (global stream only).
    sched_rx: Option<broadcast::Receiver<ScheduleChanged>>,
    /// `mcp_server_changed` notifications (global stream only).
    mcp_rx: Option<broadcast::Receiver<McpServerChanged>>,
    // ---- both ----
    storage: Arc<Storage>,
    data_dir: PathBuf,
    /// Decoded run logs kept across replay chunks (REL-04).
    logs: Arc<parking_lot::Mutex<LogCache>>,
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

    fn unavailable(&mut self) {
        self.done = true;
        self.pending.push_back(Out::Unavailable(self.delivered));
    }

    fn replay_failed(&mut self) {
        self.failures += 1;
        self.replaying = true;
        self.stalled_since.get_or_insert_with(Instant::now);
        if self.failures >= REPLAY_FAILURES {
            self.unavailable();
        } else {
            let delay = (REPLAY_BACKOFF * (1 << (self.failures - 1))).min(REPLAY_BACKOFF_MAX);
            self.retry_at = Some(tokio::time::Instant::now() + delay);
        }
    }

    fn buffer_live(&mut self, item: StreamItem) -> bool {
        if item.gseq <= self.scan || self.live.contains_key(&item.gseq) {
            return true;
        }
        let bytes = serde_json::to_vec(&item.envelope).map_or(MAX_EVENT_BYTES + 1, |v| v.len());
        if self.live.len() >= LIVE_BUFFER_EVENTS || self.live_bytes + bytes > REPLAY_BYTES {
            self.unavailable();
            return false;
        }
        self.live_bytes += bytes;
        self.live.insert(item.gseq, item);
        true
    }

    fn receive_replay_live(&mut self, received: Result<StreamItem, RecvError>) -> bool {
        match received {
            Ok(item) => self.buffer_live(item),
            Err(RecvError::Closed) => {
                self.live_closed = true;
                true
            }
            Err(RecvError::Lagged(_)) => {
                // A reconnect can recover from the persisted cursor; never
                // silently drop an overflowing subscriber's live buffer.
                self.unavailable();
                false
            }
        }
    }

    /// Backoff has a fixed deadline: incoming live traffic cannot postpone it.
    async fn wait_for_retry(&mut self, until: tokio::time::Instant) {
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => { self.done = true; return; },
                () = tokio::time::sleep_until(until) => { self.retry_at = None; return; },
                r = self.rx.recv(), if !self.live_closed => {
                    if !self.receive_replay_live(r) { return; }
                }
            }
        }
    }

    /// Global form: retry both index and worker failures without advancing the
    /// cursor. Audit every explicit gap before publishing it or its successor.
    async fn replay_chunk(&mut self) {
        let (st, dir, after) = (Arc::clone(&self.storage), self.data_dir.clone(), self.scan);
        let logs = Arc::clone(&self.logs);
        let user = self.user_id;
        let force = self.stalled_row.filter(|_| {
            self.stalled_since
                .is_some_and(|t| t.elapsed() >= PENDING_LINE_GRACE)
        });
        let mut worker = tokio::task::spawn_blocking(move || {
            let chunk = load_global(&st, &dir, after, force, &logs)?;
            #[cfg(test)]
            if !chunk.gaps.is_empty() {
                let mut cache = logs.lock();
                if cache.audit_failures > 0 {
                    cache.audit_failures -= 1;
                    return None;
                }
            }
            for gap in &chunk.gaps {
                st.append_audit(
                    user,
                    None,
                    "system",
                    "sse.gap",
                    &serde_json::to_value(gap).ok()?,
                )
                .ok()?;
            }
            // Speculative reads must not erase evidence until every gap audit
            // write succeeded. A failed commit retries from the same cursor.
            logs.lock()
                .read_errors
                .retain(|run, _| !chunk.gaps.iter().any(|gap| gap.run_id == run.0));
            Some(chunk)
        });
        let loaded = loop {
            tokio::select! {
                () = self.cancel.cancelled() => { self.done = true; return; },
                result = &mut worker => break result,
                r = self.rx.recv(), if !self.live_closed => {
                    if !self.receive_replay_live(r) { return; }
                }
            }
        };
        let Ok(Some(chunk)) = loaded else {
            self.replay_failed();
            return;
        };
        if chunk.max_gseq == after && !chunk.stalled && !self.live.is_empty() {
            // The journal broadcasts ordinary events before its index commit.
            // Wait with a bounded retry, rather than spin or pass an unseen row.
            self.replay_failed();
            return;
        }
        self.failures = 0;
        self.scan = chunk.max_gseq;
        self.replaying = chunk.more || chunk.stalled;
        if chunk.stalled {
            if self.stalled_row != chunk.stalled_gseq {
                self.stalled_since = Some(Instant::now());
            }
            self.stalled_row = chunk.stalled_gseq;
            self.retry_at = Some(tokio::time::Instant::now() + PENDING_RETRY);
        } else {
            self.stalled_since = None;
            self.stalled_row = None;
            self.retry_at = None;
        }
        let mut ordered: Vec<(u64, Out)> = chunk
            .items
            .into_iter()
            .map(|item| (item.gseq, Out::Run(item)))
            .collect();
        self.gaps_total
            .fetch_add(chunk.gaps.len() as u64, Ordering::Relaxed);
        ordered.extend(
            chunk
                .gaps
                .into_iter()
                .map(|gap| (gap.to_gseq, Out::Gap(gap))),
        );
        ordered.sort_by_key(|(gseq, _)| *gseq);
        self.pending.extend(ordered.into_iter().map(|(_, out)| out));
        self.live.retain(|gseq, _| *gseq > self.scan);
        self.live_bytes = self
            .live
            .values()
            .map(|item| serde_json::to_vec(&item.envelope).map_or(MAX_EVENT_BYTES + 1, |v| v.len()))
            .sum();
        // Items received while the index was being queried must be replayed
        // after the current chunk, never jump ahead of an unresolved row.
        self.replaying |= !self.live.is_empty();
        if !self.replaying {
            self.logs.lock().clear();
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
            if let Some(out) = self.pending.pop_front() {
                match &out {
                    Out::Run(item) => self.delivered = item.gseq,
                    Out::Gap(gap) => self.delivered = gap.to_gseq,
                    _ => {}
                }
                return Some(out);
            }
            if self.done {
                return None;
            }
            if let Some(until) = self.retry_at {
                self.wait_for_retry(until).await;
                continue;
            }
            if self.replaying {
                self.replay_chunk().await;
                continue;
            }
            if self.live_closed {
                return None;
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
                    if item.gseq > self.scan.saturating_add(1) {
                        if self.buffer_live(item) {
                            self.replaying = true;
                        }
                        continue;
                    }
                    self.scan = item.gseq;
                    self.delivered = item.gseq;
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
    Gap(Gap),
    Unavailable(u64),
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
    stalled_gseq: Option<u64>,
    gaps: Vec<Gap>,
}

/// Per-subscriber limits apply during and between chunks. Cached payloads stay
/// serialized, so the byte limit measures owned bytes rather than an estimate
/// of arbitrary nested JSON allocations. Oversized runs are streamed again.
const LOG_CACHE_RUNS: usize = 8;
const LOG_CACHE_BYTES: usize = 4 * 1024 * 1024;
const LOG_CACHE_EVENTS: usize = 8_192;
const REPLAY_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = REPLAY_BYTES;
const LIVE_BUFFER_EVENTS: usize = 1_000;
const REPLAY_FAILURES: u32 = 3;
const REPLAY_BACKOFF: Duration = Duration::from_millis(250);
const REPLAY_BACKOFF_MAX: Duration = Duration::from_secs(5);

pub(crate) const GAP_EVENT: &str = "gap";
pub(crate) const REPLAY_UNAVAILABLE: &str = "replay_unavailable";

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct Gap {
    from_gseq: u64,
    to_gseq: u64,
    run_id: Uuid,
    reason: GapReason,
}

#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum GapReason {
    LogLineMissing,
    LogUnreadable,
}

#[derive(Default)]
struct CachedLog {
    lines: HashMap<u64, Vec<u8>>,
    bytes: usize,
    max_seq: Option<u64>,
}

#[derive(Default)]
struct LogCache {
    entries: Vec<(RunId, CachedLog)>,
    reads: usize,
    /// Only unresolved read errors survive a chunk; resolved/skipped runs are removed.
    read_errors: HashMap<RunId, u32>,
    #[cfg(test)]
    index_failures: u32,
    #[cfg(test)]
    index_panics: u32,
    #[cfg(test)]
    audit_failures: u32,
}

impl LogCache {
    fn retained_bytes(&self) -> usize {
        self.entries.iter().map(|(_, log)| log.bytes).sum()
    }

    fn retained_events(&self) -> usize {
        self.entries.iter().map(|(_, log)| log.lines.len()).sum()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.read_errors.clear();
    }

    fn get(&mut self, run: RunId) -> Option<&CachedLog> {
        let i = self.entries.iter().position(|(r, _)| *r == run)?;
        let entry = self.entries.remove(i);
        self.entries.push(entry);
        self.entries.last().map(|(_, log)| log)
    }

    fn insert(&mut self, run: RunId, log: CachedLog) {
        self.entries.retain(|(r, _)| *r != run);
        while !self.entries.is_empty()
            && (self.entries.len() >= LOG_CACHE_RUNS
                || self.retained_bytes() + log.bytes > LOG_CACHE_BYTES
                || self.retained_events() + log.lines.len() > LOG_CACHE_EVENTS)
        {
            self.entries.remove(0);
        }
        self.entries.push((run, log));
    }
}

#[derive(Default)]
struct SelectedLog {
    lines: HashMap<u64, Vec<u8>>,
    present: HashSet<u64>,
    bytes: usize,
    max_seq: Option<u64>,
    unreadable: bool,
    failed_reads: u32,
}

impl SelectedLog {
    fn remember(&mut self, seq: u64, line: &[u8], budget: usize) {
        self.present.insert(seq);
        if self.bytes + line.len() <= budget {
            self.bytes += line.len();
            self.lines.insert(seq, line.to_vec());
        }
    }
}

enum LogFailure {
    Unreadable,
    /// A single event cannot fit safely. Close and preserve the cursor; never
    /// turn a resource limit into a false claim that a valid event was lost.
    Oversized,
}

/// Read one line at a time. At most one cache budget and the caller's remaining
/// chunk budget are retained, even for a many-gigabyte run or torn long line.
fn read_selected(
    dir: &std::path::Path,
    run: RunId,
    wanted: &BTreeSet<u64>,
    budget: usize,
) -> Result<(SelectedLog, Option<CachedLog>), LogFailure> {
    let file = match std::fs::File::open(storage::event_log_path(dir, run)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((SelectedLog::default(), Some(CachedLog::default())));
        }
        Err(_) => return Err(LogFailure::Unreadable),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut selected = SelectedLog::default();
    let mut complete = Some(CachedLog::default());
    loop {
        let mut line = Vec::new();
        Read::by_ref(&mut reader)
            .take((MAX_EVENT_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| LogFailure::Unreadable)?;
        if line.is_empty() {
            break;
        }
        if line.len() > MAX_EVENT_BYTES {
            return Err(LogFailure::Oversized);
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let env: Envelope = match serde_json::from_slice(&line) {
            Ok(env) => env,
            Err(_) if !line.ends_with(b"\n") => break, // torn tail
            Err(_) => return Err(LogFailure::Unreadable),
        };
        selected.max_seq = Some(selected.max_seq.map_or(env.seq, |s| s.max(env.seq)));
        if wanted.contains(&env.seq) {
            selected.remember(env.seq, &line, budget);
        }
        if let Some(log) = complete.as_mut() {
            if log.bytes + line.len() > LOG_CACHE_BYTES || log.lines.len() >= LOG_CACHE_EVENTS {
                complete = None;
            } else {
                log.bytes += line.len();
                log.max_seq = selected.max_seq;
                log.lines.insert(env.seq, line);
            }
        }
    }
    Ok((selected, complete))
}

fn select_log(
    cache: &mut LogCache,
    dir: &std::path::Path,
    run: RunId,
    wanted: &BTreeSet<u64>,
    budget: usize,
) -> Result<SelectedLog, LogFailure> {
    if let Some(log) = cache.get(run)
        && wanted.iter().all(|seq| log.lines.contains_key(seq))
    {
        let mut selected = SelectedLog {
            max_seq: log.max_seq,
            ..SelectedLog::default()
        };
        for seq in wanted {
            selected.remember(*seq, &log.lines[seq], budget);
        }
        return Ok(selected);
    }
    cache.reads += 1;
    cache.entries.retain(|(r, _)| *r != run);
    let (selected, complete) = read_selected(dir, run, wanted, budget)?;
    cache.read_errors.remove(&run);
    if let Some(log) = complete {
        cache.insert(run, log);
    }
    Ok(selected)
}

/// Index rows after `after`, with bounded streaming reads of their run logs.
/// A missing line stalls until a later line proves loss or grace expires.
/// An unreadable log requires two failed reads AND grace. Every skipped row
/// becomes an explicit gap; the caller audits gaps before committing its cursor.
fn load_global(
    st: &Storage,
    dir: &std::path::Path,
    after: u64,
    force: Option<u64>,
    cache: &parking_lot::Mutex<LogCache>,
) -> Option<Chunk> {
    #[cfg(test)]
    {
        let mut c = cache.lock();
        if c.index_panics > 0 {
            c.index_panics -= 1;
            panic!("injected index worker failure");
        }
        if c.index_failures > 0 {
            c.index_failures -= 1;
            return None;
        }
    }
    let rows = st.list_event_index(after, REPLAY_CHUNK).ok()?;
    let mut wanted: HashMap<RunId, BTreeSet<u64>> = HashMap::new();
    for row in &rows {
        wanted.entry(row.run_id).or_default().insert(row.seq);
    }
    let mut more = rows.len() >= REPLAY_CHUNK as usize;
    let mut cache = cache.lock();
    let mut selected: HashMap<RunId, SelectedLog> = HashMap::new();
    let mut selected_bytes = 0;
    let mut items = Vec::with_capacity(rows.len());
    let mut gaps = Vec::new();
    let mut max_gseq = after;
    let mut stalled_gseq = None;
    for row in rows {
        let run = row.run_id;
        if let std::collections::hash_map::Entry::Vacant(entry) = selected.entry(run) {
            let log = match select_log(
                &mut cache,
                dir,
                run,
                &wanted[&run],
                REPLAY_BYTES - selected_bytes,
            ) {
                Ok(log) => log,
                Err(LogFailure::Oversized) => return None,
                Err(LogFailure::Unreadable) => {
                    let failed_reads = cache.read_errors.entry(run).or_default();
                    *failed_reads += 1;
                    SelectedLog {
                        unreadable: true,
                        failed_reads: *failed_reads,
                        ..SelectedLog::default()
                    }
                }
            };
            selected_bytes += log.bytes;
            entry.insert(log);
        }
        let log = &selected[&run];
        if let Some(line) = log.lines.get(&row.seq) {
            let envelope = serde_json::from_slice(line).ok()?;
            items.push(StreamItem {
                gseq: row.gseq,
                run_id: run,
                envelope,
            });
        } else if log.present.contains(&row.seq) {
            // The row exists but this chunk's byte budget is full. Resume at
            // exactly this row with a fresh budget, without a gap or a stall.
            more = true;
            break;
        } else {
            let may_skip = if log.unreadable {
                force == Some(row.gseq) && log.failed_reads >= 2
            } else {
                force == Some(row.gseq) || log.max_seq.is_some_and(|seq| seq > row.seq)
            };
            if !may_skip {
                stalled_gseq = Some(row.gseq);
                more = false;
                break;
            }
            gaps.push(Gap {
                from_gseq: row.gseq,
                to_gseq: row.gseq,
                run_id: run.0,
                reason: if log.unreadable {
                    GapReason::LogUnreadable
                } else {
                    GapReason::LogLineMissing
                },
            });
        }
        max_gseq = row.gseq;
    }
    Some(Chunk {
        items,
        max_gseq,
        more,
        stalled: stalled_gseq.is_some(),
        stalled_gseq,
        gaps,
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
            Out::Gap(gap) => SseEvent::default()
                .id(gap.to_gseq.to_string())
                .event(GAP_EVENT)
                .json_data(&gap)
                .unwrap_or_else(|_| SseEvent::default().comment("unserialisable gap")),
            Out::Unavailable(last_event_id) => SseEvent::default()
                .event("error")
                .json_data(
                    serde_json::json!({"code": REPLAY_UNAVAILABLE, "last_event_id": last_event_id}),
                )
                .unwrap_or_else(|_| SseEvent::default().comment(REPLAY_UNAVAILABLE)),
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
        stalled_row: None,
        failures: 0,
        retry_at: None,
        pending: VecDeque::new(),
        live: BTreeMap::new(),
        live_bytes: 0,
        live_closed: false,
        delivered: 0,
        user_id: state.user_id(),
        gaps_total: Arc::clone(&state.0.sse_gaps_total),
        sched_rx: scoped_run
            .is_none()
            .then(|| state.subscribe_schedule_events()),
        mcp_rx: scoped_run
            .is_none()
            .then(|| state.mcp().map(|m| m.subscribe()))
            .flatten(),
        storage: state.storage_arc(),
        data_dir: state.data_dir(),
        logs: Arc::default(),
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
                .map_err(ApiError::internal)?
                .map_err(ApiError::internal)?;
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
            feed.delivered = feed.scan;
        }
    }
    Ok(Sse::new(event_stream(feed)).keep_alive(KeepAlive::default()))
}

/// Liveness plus the count of explicit SSE gap notifications in this daemon.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ReplayHealth {
    #[serde(flatten)]
    health: crate::dto::Health,
    /// Gap notifications audited by this daemon (per subscriber, since startup).
    sse_gaps_total: u64,
}

#[utoipa::path(get, path = "/v1/health", tag = "meta",
    responses((status = 200, body = ReplayHealth)))]
pub(crate) async fn health(State(state): State<AppState>) -> axum::Json<ReplayHealth> {
    let total = state.0.sse_gaps_total.load(Ordering::Relaxed);
    let base_health = crate::routes::health;
    let axum::Json(health) = base_health(State(state)).await;
    axum::Json(ReplayHealth {
        health,
        sse_gaps_total: total,
    })
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
        let chunk = load_global(&st, dir.path(), 0, None, &LogCache::default().into()).unwrap();
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
        let forced = load_global(&st, dir.path(), 3, Some(4), &LogCache::default().into()).unwrap();
        assert_eq!(forced.items.len(), 1);
        assert_eq!(forced.items[0].gseq, 5);
        assert!(!forced.stalled);
        assert_eq!(forced.max_gseq, 5);
        let none = load_global(&st, dir.path(), 5, None, &LogCache::default().into()).unwrap();
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
        let cache = parking_lot::Mutex::new(LogCache::default());
        let first = load_global(&st, dir.path(), 0, None, &cache).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!((first.max_gseq, first.stalled), (1, true));
        // The line lands; resuming from the stalled cursor delivers it.
        w.append(&msg(a, 1)).unwrap();
        let second = load_global(&st, dir.path(), first.max_gseq, None, &cache).unwrap();
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
        let chunk = load_global(&st, dir.path(), 0, None, &LogCache::default().into()).unwrap();
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
        let chunk = load_global(&st, dir.path(), 0, None, &LogCache::default().into()).unwrap();
        assert!(chunk.items.is_empty());
        assert_eq!((chunk.max_gseq, chunk.stalled), (0, true));
    }

    #[test]
    fn a_long_run_is_decoded_once_across_replay_chunks() {
        // REL-04: 5.2k events of one run replayed in 500-row chunks.
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let a = RunId::new();
        let mut w = storage::EventLogWriter::open(dir.path(), a).unwrap();
        let total = 5_200u64;
        for seq in 0..total {
            w.append(&msg(a, seq)).unwrap();
            st.index_event(seq + 1, a, seq).unwrap();
        }
        let cache = parking_lot::Mutex::new(LogCache::default());
        let (mut after, mut delivered, mut chunks) = (0, 0u64, 0);
        loop {
            let c = load_global(&st, dir.path(), after, None, &cache).unwrap();
            delivered += c.items.len() as u64;
            after = c.max_gseq;
            chunks += 1;
            assert!(!c.stalled);
            if !c.more {
                break;
            }
        }
        assert_eq!(delivered, total);
        assert!(chunks >= 11, "{chunks}");
        assert_eq!(cache.lock().reads, 1, "the log must be decoded once");
    }

    #[test]
    fn log_cache_is_bounded_and_refreshes_a_stale_log_once() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let cache = parking_lot::Mutex::new(LogCache::default());
        for g in 1..=(LOG_CACHE_RUNS as u64 + 4) {
            let r = RunId::new();
            let mut w = storage::EventLogWriter::open(dir.path(), r).unwrap();
            w.append(&msg(r, 0)).unwrap();
            st.index_event(g, r, 0).unwrap();
        }
        let c = load_global(&st, dir.path(), 0, None, &cache).unwrap();
        assert_eq!(c.items.len(), LOG_CACHE_RUNS + 4);
        assert_eq!(cache.lock().entries.len(), LOG_CACHE_RUNS);
    }

    #[test]
    fn a_chunk_touching_more_runs_than_the_cache_holds_neither_stalls_nor_drops() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let runs: Vec<RunId> = (0..12).map(|_| RunId::new()).collect();
        let mut writers: Vec<_> = runs
            .iter()
            .map(|r| storage::EventLogWriter::open(dir.path(), *r).unwrap())
            .collect();
        let mut g = 0;
        for seq in 0..2 {
            for (i, r) in runs.iter().enumerate() {
                writers[i].append(&msg(*r, seq)).unwrap();
                g += 1;
                st.index_event(g, *r, seq).unwrap();
            }
        }
        for force in [false, true] {
            let cache = parking_lot::Mutex::new(LogCache::default());
            let c = load_global(&st, dir.path(), 0, force.then_some(1), &cache).unwrap();
            assert!(!c.stalled, "force={force}");
            assert_eq!(c.items.len(), 24, "force={force}");
            assert_eq!(c.max_gseq, 24);
            assert_eq!(cache.lock().reads, 12, "each log decoded once");
            assert_eq!(cache.lock().entries.len(), LOG_CACHE_RUNS);
        }
    }

    fn replay_fixture() -> (
        tempfile::TempDir,
        Arc<Storage>,
        Feed,
        broadcast::Sender<StreamItem>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let user_id = protocol::UserId::new();
        storage
            .insert_user(&storage::User {
                id: user_id,
                name: "SSE fixture".into(),
                created_at_ms: 0,
            })
            .unwrap();
        let (tx, rx) = broadcast::channel(2_048);
        let feed = Feed {
            rx,
            backlog: VecDeque::new(),
            scoped: false,
            run: None,
            last: None,
            done: false,
            finished_in_log: false,
            scan: 0,
            replaying: true,
            stalled_since: None,
            stalled_row: None,
            failures: 0,
            retry_at: None,
            pending: VecDeque::new(),
            live: BTreeMap::new(),
            live_bytes: 0,
            live_closed: false,
            delivered: 0,
            user_id,
            gaps_total: Arc::default(),
            sched_rx: None,
            mcp_rx: None,
            storage: Arc::clone(&storage),
            data_dir: dir.path().to_owned(),
            logs: Arc::default(),
            cancel: CancellationToken::new(),
            _app: None,
        };
        (dir, storage, feed, tx)
    }

    fn append_indexed(st: &Storage, run: RunId, gseq: u64, seq: u64) -> StreamItem {
        let envelope = msg(run, seq);
        storage::EventLogWriter::open(st.data_dir(), run)
            .unwrap()
            .append(&envelope)
            .unwrap();
        st.index_event(gseq, run, seq).unwrap();
        StreamItem {
            gseq,
            run_id: run,
            envelope,
        }
    }

    #[tokio::test]
    async fn transient_index_failures_preserve_live_order_exactly_once() {
        let (_dir, st, mut feed, tx) = replay_fixture();
        let run = RunId::new();
        append_indexed(&st, run, 1, 0);
        append_indexed(&st, run, 2, 1);
        feed.logs.lock().index_failures = 2;
        let producer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            for (gseq, seq) in [(3, 2), (4, 3)] {
                tx.send(append_indexed(&st, run, gseq, seq)).unwrap();
            }
        });
        let mut ids = Vec::new();
        for _ in 0..4 {
            let out = tokio::time::timeout(Duration::from_secs(3), feed.next_global_item())
                .await
                .unwrap();
            let Some(Out::Run(item)) = out else {
                panic!("expected an ordered replay event")
            };
            ids.push(item.gseq);
        }
        producer.await.unwrap();
        assert_eq!(ids, [1, 2, 3, 4]);
        assert_eq!((feed.scan, feed.delivered, feed.failures), (4, 4, 0));
        assert!(feed.live.is_empty());
        assert!(
            feed.next_global_item().await.is_none(),
            "closed source has no duplicate events"
        );
    }

    #[tokio::test]
    async fn replay_failure_keeps_cursor_and_replaying_until_terminal_error() {
        let (_dir, _st, mut feed, _tx) = replay_fixture();
        feed.scan = 7;
        feed.delivered = 7;
        feed.logs.lock().index_failures = REPLAY_FAILURES;
        feed.replay_chunk().await;
        assert_eq!(feed.scan, 7);
        assert!(
            feed.replaying,
            "BUG-16: storage failure must not switch to live delivery"
        );
        assert!(feed.stalled_since.is_some());
        let started = Instant::now();
        let out = tokio::time::timeout(Duration::from_secs(3), feed.next_global_item())
            .await
            .unwrap();
        assert!(matches!(out, Some(Out::Unavailable(7))));
        assert!(
            started.elapsed() >= Duration::from_millis(700),
            "exponential retry backoff"
        );
        assert_eq!((feed.scan, feed.delivered, feed.failures), (7, 7, 3));
        assert!(feed.next_global_item().await.is_none());
    }

    #[tokio::test]
    async fn replay_worker_join_failure_retries_from_the_same_cursor() {
        let (_dir, st, mut feed, _tx) = replay_fixture();
        append_indexed(&st, RunId::new(), 1, 0);
        feed.logs.lock().index_panics = 1;
        feed.replay_chunk().await;
        assert_eq!(feed.scan, 0);
        assert!(feed.replaying);
        let out = tokio::time::timeout(Duration::from_secs(2), feed.next_global_item())
            .await
            .unwrap();
        assert!(matches!(out, Some(Out::Run(StreamItem { gseq: 1, .. }))));
    }

    #[tokio::test]
    async fn missing_line_grace_emits_an_audited_gap_before_its_successor() {
        let (_dir, st, mut feed, _tx) = replay_fixture();
        let lost = RunId::new();
        st.index_event(1, lost, 0).unwrap();
        append_indexed(&st, RunId::new(), 2, 0);
        feed.replay_chunk().await;
        assert_eq!(feed.scan, 0);
        assert!(feed.pending.is_empty());
        assert_eq!(feed.gaps_total.load(Ordering::Relaxed), 0);
        feed.stalled_since = Some(Instant::now().checked_sub(PENDING_LINE_GRACE).unwrap());
        feed.replay_chunk().await;
        let audit = st.list_audit(feed.user_id).unwrap();
        assert_eq!(audit.len(), 1, "gap is durable before it can be yielded");
        assert_eq!(audit[0].action, "sse.gap");
        assert_eq!(audit[0].detail["reason"], "log_line_missing");
        assert!(matches!(
            feed.next_global_item().await,
            Some(Out::Gap(Gap {
                from_gseq: 1,
                to_gseq: 1,
                ..
            }))
        ));
        assert!(matches!(
            feed.next_global_item().await,
            Some(Out::Run(StreamItem { gseq: 2, .. }))
        ));
        assert_eq!(feed.gaps_total.load(Ordering::Relaxed), 1);
        assert_eq!(feed.delivered, 2);
    }

    #[test]
    fn unreadable_log_needs_two_attempts_even_when_grace_is_expired() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let run = RunId::new();
        let path = storage::event_log_path(dir.path(), run);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"broken\n").unwrap();
        for seq in 0..2 {
            st.index_event(seq + 1, run, seq).unwrap();
        }
        let cache = parking_lot::Mutex::new(LogCache::default());
        let first = load_global(&st, dir.path(), 0, Some(1), &cache).unwrap();
        assert!(first.stalled && first.gaps.is_empty());
        assert_eq!(first.max_gseq, 0);
        let second = load_global(&st, dir.path(), 0, Some(1), &cache).unwrap();
        assert!(second.stalled);
        assert_eq!(second.max_gseq, 1);
        assert_eq!(second.gaps.len(), 1);
        assert!(
            second
                .gaps
                .iter()
                .all(|gap| matches!(gap.reason, GapReason::LogUnreadable))
        );
    }

    #[tokio::test]
    async fn stalled_live_buffer_overflow_closes_without_advancing_cursor() {
        let (_dir, _st, mut feed, tx) = replay_fixture();
        feed.logs.lock().index_failures = REPLAY_FAILURES;
        let run = RunId::new();
        for gseq in 1..=LIVE_BUFFER_EVENTS as u64 + 1 {
            tx.send(StreamItem {
                gseq,
                run_id: run,
                envelope: msg(run, gseq),
            })
            .unwrap();
        }
        let out = tokio::time::timeout(Duration::from_secs(2), feed.next_global_item())
            .await
            .unwrap();
        assert!(matches!(out, Some(Out::Unavailable(0))));
        assert_eq!(feed.scan, 0);
        assert!(feed.live.len() <= LIVE_BUFFER_EVENTS);
        assert!(feed.next_global_item().await.is_none());
    }

    #[test]
    fn oversized_run_is_reread_with_bounded_cache_without_stalls_or_skips() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let run = RunId::new();
        let mut writer = storage::EventLogWriter::open(dir.path(), run).unwrap();
        let count = 1_101u64;
        for seq in 0..count {
            let env = Envelope::new(
                seq,
                Event::MessageDelta(protocol::event::MessageDelta {
                    run_id: run,
                    text: "x".repeat(8_192),
                }),
            );
            writer.append_buffered(&env).unwrap();
            st.index_event(seq + 1, run, seq).unwrap();
        }
        writer.sync().unwrap();
        drop(writer);
        let cache = parking_lot::Mutex::new(LogCache::default());
        let (mut after, mut chunks) = (0, 0);
        let mut got = Vec::new();
        loop {
            let chunk = load_global(&st, dir.path(), after, None, &cache).unwrap();
            assert!(!chunk.stalled && chunk.gaps.is_empty());
            assert!(chunk.max_gseq > after);
            after = chunk.max_gseq;
            got.extend(chunk.items.iter().map(|item| item.gseq));
            chunks += 1;
            assert!(
                cache.lock().retained_bytes() <= LOG_CACHE_BYTES,
                "REL-04: oversized decoded log retained across chunks"
            );
            assert!(cache.lock().retained_events() <= LOG_CACHE_EVENTS);
            if !chunk.more {
                break;
            }
        }
        assert_eq!(got, (1..=count).collect::<Vec<_>>());
        assert_eq!(cache.lock().reads, chunks);
        assert!(chunks >= 3);
        assert_eq!(
            cache.lock().retained_bytes(),
            0,
            "oversized logs are never retained"
        );
    }

    #[test]
    fn chunk_byte_budget_restarts_at_the_first_unreturned_row() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let run = RunId::new();
        let mut writer = storage::EventLogWriter::open(dir.path(), run).unwrap();
        for seq in 0..4 {
            writer
                .append_buffered(&Envelope::new(
                    seq,
                    Event::MessageDelta(protocol::event::MessageDelta {
                        run_id: run,
                        text: "x".repeat(3 * 1024 * 1024),
                    }),
                ))
                .unwrap();
            st.index_event(seq + 1, run, seq).unwrap();
        }
        writer.sync().unwrap();
        drop(writer);
        let cache = parking_lot::Mutex::new(LogCache::default());
        let first = load_global(&st, dir.path(), 0, None, &cache).unwrap();
        assert_eq!(first.max_gseq, 2);
        assert!(first.more && !first.stalled && first.gaps.is_empty());
        let second = load_global(&st, dir.path(), first.max_gseq, None, &cache).unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.gseq)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(second.max_gseq, 4);
        assert!(!second.more && !second.stalled && second.gaps.is_empty());
    }

    #[tokio::test]
    async fn expired_gap_does_not_force_a_new_pending_row() {
        let (_dir, st, mut feed, _tx) = replay_fixture();
        st.index_event(1, RunId::new(), 0).unwrap();
        st.index_event(2, RunId::new(), 0).unwrap();
        feed.replay_chunk().await;
        feed.stalled_since = Some(Instant::now().checked_sub(PENDING_LINE_GRACE).unwrap());
        feed.replay_chunk().await;
        assert_eq!(feed.scan, 1);
        assert_eq!(feed.stalled_row, Some(2));
        assert_eq!(feed.pending.len(), 1);
        assert!(matches!(
            feed.next_global_item().await,
            Some(Out::Gap(Gap { to_gseq: 1, .. }))
        ));
        assert!(feed.stalled_since.unwrap().elapsed() < PENDING_LINE_GRACE);
    }

    #[tokio::test]
    async fn buffered_live_waits_for_the_index_without_spinning() {
        let (_dir, st, mut feed, _tx) = replay_fixture();
        let run = RunId::new();
        let item = StreamItem {
            gseq: 1,
            run_id: run,
            envelope: msg(run, 0),
        };
        feed.buffer_live(item);
        feed.replay_chunk().await;
        assert_eq!((feed.scan, feed.failures), (0, 1));
        assert!(feed.retry_at.is_some());
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            append_indexed(&st, run, 1, 0);
        });
        let out = tokio::time::timeout(Duration::from_secs(2), feed.next_global_item())
            .await
            .unwrap();
        writer.await.unwrap();
        assert!(matches!(out, Some(Out::Run(StreamItem { gseq: 1, .. }))));
        assert_eq!(feed.failures, 0);
        assert!(feed.live.is_empty());
    }

    #[tokio::test]
    async fn permanently_unindexed_live_event_closes_at_the_old_cursor() {
        let (_dir, _st, mut feed, _tx) = replay_fixture();
        let run = RunId::new();
        feed.buffer_live(StreamItem {
            gseq: 1,
            run_id: run,
            envelope: msg(run, 0),
        });
        let out = tokio::time::timeout(Duration::from_secs(2), feed.next_global_item())
            .await
            .unwrap();
        assert!(matches!(out, Some(Out::Unavailable(0))));
        assert_eq!((feed.scan, feed.failures), (0, 3));
        assert!(feed.next_global_item().await.is_none());
    }

    #[tokio::test]
    async fn persistent_gap_audit_failure_closes_without_losing_read_evidence() {
        let (_dir, st, mut feed, _tx) = replay_fixture();
        let run = RunId::new();
        st.index_event(1, run, 0).unwrap();
        let path = storage::event_log_path(st.data_dir(), run);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"broken\n").unwrap();
        feed.replay_chunk().await;
        feed.stalled_since = Some(Instant::now().checked_sub(PENDING_LINE_GRACE).unwrap());
        feed.logs.lock().audit_failures = 3;
        let out = tokio::time::timeout(Duration::from_secs(2), feed.next_global_item())
            .await
            .unwrap();
        assert!(matches!(out, Some(Out::Unavailable(0))));
        assert_eq!((feed.scan, feed.failures), (0, 3));
        assert_eq!(feed.gaps_total.load(Ordering::Relaxed), 0);
        assert_eq!(
            st.list_audit(feed.user_id).unwrap(),
            Vec::<storage::AuditEntry>::new()
        );
    }
}
