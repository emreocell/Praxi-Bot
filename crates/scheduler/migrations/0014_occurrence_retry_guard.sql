-- Retry bookkeeping per occurrence. `attempt` is the index of the run attempt
-- in its durable idempotency key (`schedule:due:attempt`), `retry_count` counts
-- manual retries (capped) and `last_retry_ms` drives the retry cooldown.
ALTER TABLE occurrences ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE occurrences ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE occurrences ADD COLUMN last_retry_ms INTEGER;
