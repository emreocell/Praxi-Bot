-- A CAPTCHA a mission session parked for the owner on the live page (one row per tool call).
-- The page itself lives in the bot's browser; this row is what outlives the session that asked
-- and a restart, so the supervisor can expire it and the owner's answer can find its item.
-- `claimed_at_ms` is set while one owner answer is checking the page: expiry leaves it alone.

CREATE TABLE mission_handovers (
    call_id        TEXT PRIMARY KEY,
    mission_id     TEXT NOT NULL REFERENCES missions(id),
    item_key       TEXT NOT NULL,
    bot_id         TEXT NOT NULL,
    run_id         TEXT NOT NULL,
    kind           TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL,
    claimed_at_ms  INTEGER
);

CREATE INDEX mission_handovers_expiry ON mission_handovers(expires_at_ms);
CREATE INDEX mission_handovers_mission ON mission_handovers(mission_id);
