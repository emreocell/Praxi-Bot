ALTER TABLE schedules ADD COLUMN revision INTEGER NOT NULL DEFAULT 0 CHECK (typeof(revision) = 'integer' AND revision >= 0);

-- The identity of a timer occurrence is schedule_id || ':' || due_ms. A
-- dispatched row is deliberately NOT called started: the legacy executor
-- cannot return a durably deduplicated run_id yet (task 136).
CREATE TABLE occurrences (
    schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    due_ms      INTEGER NOT NULL,
    revision    INTEGER NOT NULL,
    run_id      TEXT,
    state       TEXT NOT NULL CHECK (state IN ('pending', 'dispatched', 'succeeded', 'failed', 'started')),
    PRIMARY KEY (schedule_id, due_ms)
);

CREATE INDEX idx_occurrences_pending ON occurrences(schedule_id, state);
