//! Background event journal: callers enqueue records and return immediately; a
//! dedicated thread writes them in `gseq` order and makes a whole batch of
//! records (typically many `MessageDelta` lines) durable with a single `fsync`.
//! Every record keeps its own log line and `seq`: clients detect gaps by `seq`,
//! so deltas are batched for durability, never merged.
//!
//! [`EventJournal::flush`] is the synchronous barrier: when it returns, every
//! record enqueued before the call is durable.  Callers use it for approval and
//! run-state events and must not hold their own global locks while waiting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

use protocol::{Envelope, RunId};

use crate::db::Storage;
use crate::error::Result;
use crate::event_log::EventLogWriter;

/// Default bound of the queue (records waiting for the writer thread).
pub const DEFAULT_CAPACITY: usize = 8192;
/// Most queue messages handled per write/sync cycle.
const MAX_BATCH: usize = 2048;
/// Open run logs kept by [`FsJournalSink`] between batches.
const MAX_OPEN_LOGS: usize = 32;

/// One event to journal: its global sequence number, run and envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRecord {
    /// Global sequence number (index key).
    pub gseq: u64,
    /// Run the event belongs to.
    pub run_id: RunId,
    /// The event as it appears in the run's log.
    pub envelope: Envelope,
}

/// Destination of the journal thread.
pub trait JournalSink: Send + 'static {
    /// Accept one record (may buffer).
    ///
    /// # Errors
    /// Sink-specific failures; the journal counts and skips them.
    fn write(&mut self, rec: &JournalRecord) -> Result<()>;

    /// Make everything written so far durable.
    ///
    /// # Errors
    /// Sink-specific failures.
    fn sync(&mut self) -> Result<()>;
}

enum Msg {
    Rec(Box<JournalRecord>),
    Barrier(SyncSender<()>),
}

/// Bounded asynchronous journal writer (see the module docs).
#[derive(Debug)]
pub struct EventJournal {
    tx: Option<SyncSender<Msg>>,
    handle: Option<JoinHandle<()>>,
    errors: Arc<AtomicU64>,
}

impl EventJournal {
    /// Journal into `sink` with the default queue bound.
    #[must_use]
    pub fn new(sink: impl JournalSink) -> Self {
        Self::with_capacity(sink, DEFAULT_CAPACITY)
    }

    /// Journal into `sink`; at most `capacity` records wait in the queue
    /// (producers block, never drop, when it is full).
    #[must_use]
    pub fn with_capacity(sink: impl JournalSink, capacity: usize) -> Self {
        let (tx, rx) = sync_channel(capacity.max(1));
        let errors = Arc::new(AtomicU64::new(0));
        let thread_errors = Arc::clone(&errors);
        let handle = std::thread::Builder::new()
            .name("event-journal".into())
            .spawn(move || writer_loop(&rx, sink, &thread_errors))
            .map_err(|e| {
                eprintln!(
                    "error: event journal thread did not start: {e}; events are NOT persisted"
                );
                errors.fetch_add(1, Ordering::Relaxed);
            })
            .ok();
        Self {
            tx: Some(tx),
            handle,
            errors,
        }
    }

    /// Journal into the event index of `storage` and the NDJSON run logs under
    /// its data directory.
    #[must_use]
    pub fn fs(storage: Arc<Storage>) -> Self {
        Self::new(FsJournalSink::new(storage))
    }

    /// Queue a record.  Returns immediately unless the queue is full.
    pub fn append(&self, rec: JournalRecord) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Rec(Box::new(rec)));
        }
    }

    /// Barrier: returns once every record queued before this call is durable.
    pub fn flush(&self) {
        let Some(tx) = &self.tx else { return };
        let (ack_tx, ack_rx) = sync_channel(1);
        if tx.send(Msg::Barrier(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// Number of sink write/sync failures so far.
    #[must_use]
    pub fn error_count(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

impl Drop for EventJournal {
    fn drop(&mut self) {
        // Closing the channel lets the thread drain what is queued and exit.
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Count a sink failure; the first one is also logged.
fn note_error(errors: &AtomicU64, what: &str, e: &crate::error::StorageError) {
    if errors.fetch_add(1, Ordering::Relaxed) == 0 {
        eprintln!("error: event journal {what} failed: {e} (further failures are only counted)");
    }
}

fn writer_loop(rx: &Receiver<Msg>, mut sink: impl JournalSink, errors: &AtomicU64) {
    while let Ok(first) = rx.recv() {
        let mut msgs = vec![first];
        let mut closed = false;
        while msgs.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(m) => msgs.push(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        let mut barriers = Vec::new();
        for m in msgs {
            match m {
                Msg::Rec(rec) => {
                    if let Err(e) = sink.write(&rec) {
                        note_error(errors, "write", &e);
                    }
                }
                Msg::Barrier(b) => barriers.push(b),
            }
        }
        if let Err(e) = sink.sync() {
            note_error(errors, "sync", &e);
        }
        for b in barriers {
            let _ = b.send(());
        }
        if closed {
            break;
        }
    }
}

/// The production sink: the SQLite event index plus per-run NDJSON logs.
#[derive(Debug)]
pub struct FsJournalSink {
    storage: Arc<Storage>,
    data_dir: PathBuf,
    logs: HashMap<RunId, (EventLogWriter, bool)>,
}

impl FsJournalSink {
    /// Sink writing under `storage`'s data directory.
    #[must_use]
    pub fn new(storage: Arc<Storage>) -> Self {
        let data_dir = storage.data_dir().to_path_buf();
        Self {
            storage,
            data_dir,
            logs: HashMap::new(),
        }
    }
}

impl JournalSink for FsJournalSink {
    fn write(&mut self, rec: &JournalRecord) -> Result<()> {
        // Log line first, index row second: a concurrent replay that sees an
        // index row always finds its line (the writer thread lags the emitter).
        // A crash in between leaves a line without an index row, which only
        // the per-run replay shows.  An index failure must not stop the log.
        if !self.logs.contains_key(&rec.run_id) {
            let w = EventLogWriter::open(&self.data_dir, rec.run_id)?;
            self.logs.insert(rec.run_id, (w, false));
        }
        if let Some((w, dirty)) = self.logs.get_mut(&rec.run_id) {
            w.append_buffered(&rec.envelope)?;
            *dirty = true;
        }
        // The line is already written; a failing index row is reported (counted
        // and logged by the journal), not ignored.
        self.storage
            .index_event(rec.gseq, rec.run_id, rec.envelope.seq)
    }

    fn sync(&mut self) -> Result<()> {
        let mut first_err = None;
        for (w, dirty) in self.logs.values_mut() {
            if std::mem::take(dirty)
                && let Err(e) = w.sync()
            {
                first_err.get_or_insert(e);
            }
        }
        if self.logs.len() > MAX_OPEN_LOGS {
            self.logs.clear();
        }
        first_err.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{event_log_path, read_event_log};
    use protocol::Event;
    use protocol::event::{MessageDelta, TodoUpdated};
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn delta(run: RunId, gseq: u64, seq: u64, text: &str) -> JournalRecord {
        JournalRecord {
            gseq,
            run_id: run,
            envelope: Envelope::new(
                seq,
                Event::MessageDelta(MessageDelta {
                    run_id: run,
                    text: text.into(),
                }),
            ),
        }
    }

    fn todo(run: RunId, gseq: u64, seq: u64) -> JournalRecord {
        JournalRecord {
            gseq,
            run_id: run,
            envelope: Envelope::new(
                seq,
                Event::TodoUpdated(TodoUpdated {
                    run_id: run,
                    items: vec![],
                }),
            ),
        }
    }

    /// Records everything; `sync` can be slow.
    #[derive(Clone, Default)]
    struct Probe {
        written: Arc<Mutex<Vec<JournalRecord>>>,
        synced: Arc<Mutex<usize>>,
        sync_delay: Duration,
    }

    impl JournalSink for Probe {
        fn write(&mut self, rec: &JournalRecord) -> Result<()> {
            self.written.lock().unwrap().push(rec.clone());
            Ok(())
        }
        fn sync(&mut self) -> Result<()> {
            std::thread::sleep(self.sync_delay);
            *self.synced.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn append_does_not_block_on_a_slow_fsync_but_flush_waits() {
        let probe = Probe {
            sync_delay: Duration::from_millis(300),
            ..Probe::default()
        };
        let journal = EventJournal::new(probe.clone());
        let run = RunId::new();
        let t = Instant::now();
        for i in 0..200 {
            journal.append(todo(run, i + 1, i));
        }
        assert!(
            t.elapsed() < Duration::from_millis(250),
            "append blocked on fsync: {:?}",
            t.elapsed()
        );
        journal.flush();
        assert!(t.elapsed() >= Duration::from_millis(300));
        assert_eq!(
            probe.written.lock().unwrap().len(),
            200,
            "barrier returned before records were written"
        );
        // Batched: far fewer syncs than records.
        assert!(*probe.synced.lock().unwrap() < 20);
    }

    #[test]
    fn barrier_means_all_prior_records_are_written_and_synced() {
        let probe = Probe {
            sync_delay: Duration::from_millis(20),
            ..Probe::default()
        };
        let journal = EventJournal::new(probe.clone());
        let run = RunId::new();
        for round in 0..5u64 {
            journal.append(todo(run, round * 2 + 1, round * 2));
            journal.append(todo(run, round * 2 + 2, round * 2 + 1));
            journal.flush();
            assert_eq!(probe.written.lock().unwrap().len() as u64, (round + 1) * 2);
            assert!(*probe.synced.lock().unwrap() > 0);
        }
    }

    #[test]
    fn deltas_are_batched_but_keep_every_seq_and_gseq_order() {
        let probe = Probe {
            sync_delay: Duration::from_millis(100),
            ..Probe::default()
        };
        let journal = EventJournal::new(probe.clone());
        let run = RunId::new();
        // Keep the writer busy so the rest queue into one batch.
        journal.append(todo(run, 1, 0));
        std::thread::sleep(Duration::from_millis(30));
        for i in 1..=20u64 {
            journal.append(delta(run, i + 1, i, "x"));
        }
        journal.flush();
        let written = probe.written.lock().unwrap();
        let seqs: Vec<u64> = written.iter().map(|r| r.envelope.seq).collect();
        assert_eq!(seqs, (0..=20).collect::<Vec<_>>(), "no seq gap, no merge");
        // 21 records, but far fewer than 21 syncs.
        assert!(*probe.synced.lock().unwrap() <= 4);
    }

    #[test]
    fn concurrent_producers_keep_gseq_order_and_all_records() {
        let probe = Probe::default();
        let journal = Arc::new(EventJournal::new(probe.clone()));
        let gate = Arc::new(Mutex::new(0u64));
        let mut hs = Vec::new();
        for _ in 0..4 {
            let (journal, gate) = (Arc::clone(&journal), Arc::clone(&gate));
            hs.push(std::thread::spawn(move || {
                let run = RunId::new();
                for seq in 0..100u64 {
                    // Like `emit`: take the gseq and enqueue under one lock.
                    let mut g = gate.lock().unwrap();
                    *g += 1;
                    journal.append(todo(run, *g, seq));
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        journal.flush();
        let written = probe.written.lock().unwrap();
        assert_eq!(written.len(), 400);
        let gs: Vec<u64> = written.iter().map(|r| r.gseq).collect();
        assert!(gs.windows(2).all(|p| p[0] < p[1]), "gseq not monotonic");
    }

    struct FailSink;

    impl JournalSink for FailSink {
        fn write(&mut self, _: &JournalRecord) -> Result<()> {
            Err(std::io::Error::other("disk full").into())
        }
        fn sync(&mut self) -> Result<()> {
            Err(std::io::Error::other("disk full").into())
        }
    }

    // Review #10: sink failures are counted (and the first one logged), and a
    // barrier still returns instead of hanging.
    #[test]
    fn sink_failures_are_counted_and_barrier_still_returns() {
        let journal = EventJournal::new(FailSink);
        assert_eq!(journal.error_count(), 0);
        journal.append(todo(RunId::new(), 1, 0));
        journal.flush();
        assert!(journal.error_count() >= 2, "{}", journal.error_count());
    }

    #[test]
    fn fs_journal_replays_after_torn_tail_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let run = RunId::new();
        {
            let journal = EventJournal::fs(Arc::clone(&storage));
            for i in 0..5u64 {
                journal.append(todo(run, i + 1, i));
            }
            journal.flush();
        }
        assert_eq!(read_event_log(dir.path(), run).unwrap().len(), 5);
        // Crash mid-write: run_a torn final line.
        let path = event_log_path(dir.path(), run);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"v\":1,\"seq\":5,\"te").unwrap();
        drop(f);
        assert_eq!(read_event_log(dir.path(), run).unwrap().len(), 5);
        // A new journal (restart) truncates the tail and appends cleanly.
        {
            let journal = EventJournal::fs(Arc::clone(&storage));
            journal.append(todo(run, 6, 5));
            journal.append(todo(run, 7, 6));
        } // drop drains the queue
        let log = read_event_log(dir.path(), run).unwrap();
        assert_eq!(
            log.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6]
        );
        assert_eq!(storage.list_event_index(0, 100).unwrap().len(), 7);
    }
}
