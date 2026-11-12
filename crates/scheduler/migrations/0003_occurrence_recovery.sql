-- Widen the occurrence state set with the two terminal states used by startup
-- recovery: `missed` (reserved but never dispatched) and `interrupted`
-- (dispatched, outcome unknown). SQLite cannot alter a CHECK constraint, so the
-- table is rebuilt; every existing row is copied unchanged.
CREATE TABLE occurrences_new (
    schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    due_ms      INTEGER NOT NULL,
    revision    INTEGER NOT NULL,
    run_id      TEXT,
    state       TEXT NOT NULL CHECK (state IN ('pending', 'dispatched', 'succeeded', 'failed', 'started', 'missed', 'interrupted')),
    PRIMARY KEY (schedule_id, due_ms)
);

INSERT INTO occurrences_new (schedule_id, due_ms, revision, run_id, state)
    SELECT schedule_id, due_ms, revision, run_id, state FROM occurrences;

DROP TABLE occurrences;
ALTER TABLE occurrences_new RENAME TO occurrences;
CREATE INDEX idx_occurrences_pending ON occurrences(schedule_id, state);
