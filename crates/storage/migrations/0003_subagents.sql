-- Subagents: a child run points at the run that spawned it and keeps its
-- spawn parameters (task, tool list, model role, limits, idempotency key) so a
-- restart can rebuild it.  Top-level runs leave both columns NULL.
ALTER TABLE runs ADD COLUMN parent_run_id TEXT REFERENCES runs(id);
ALTER TABLE runs ADD COLUMN spec_json TEXT;
CREATE INDEX idx_runs_parent ON runs(parent_run_id);
