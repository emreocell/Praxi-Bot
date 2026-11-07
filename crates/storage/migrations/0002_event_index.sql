-- Daemon-wide event order: every emitted run event gets the next `gseq`
-- (monotonic, persisted, never reused), so a global SSE stream can resume
-- with one cursor across all runs.  The envelope itself lives in the run's
-- NDJSON log; this table only maps `gseq` to `(run_id, seq)`.
CREATE TABLE event_index (
    gseq   INTEGER PRIMARY KEY,
    run_id TEXT    NOT NULL,
    seq    INTEGER NOT NULL
);
CREATE INDEX idx_event_index_run ON event_index(run_id, seq);
