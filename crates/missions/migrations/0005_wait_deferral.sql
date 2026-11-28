-- Missions wave 3: a mail wait that was put off because the person wrote back "out of office".
-- Forward-only. `not_before_ms`: mail from before this moment is not the awaited reply (the
-- out-of-office notice that caused the deferral must not be found again). `defers`: how many times
-- the wait was put off, so a chain of notices cannot park an item for ever.
ALTER TABLE mission_waits ADD COLUMN not_before_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mission_waits ADD COLUMN defers INTEGER NOT NULL DEFAULT 0;
