-- Missions wave 2: items that wait for a wake (timer, mail, webhook) and the wakes that end the wait.
-- Forward-only, like 0001. Waits and wakes survive a restart; the supervisor's wake loop (leader only)
-- reads them.

-- One registered wait. `after`: `due_at_ms` is when it fires. `mail` and `webhook`: it is when it gives up.
CREATE TABLE mission_waits (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id       TEXT NOT NULL REFERENCES missions(id),
    item_key         TEXT NOT NULL,
    kind             TEXT NOT NULL,
    spec_json        TEXT NOT NULL,
    reason           TEXT NOT NULL,
    state            TEXT NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    due_at_ms        INTEGER NOT NULL,
    next_poll_at_ms  INTEGER NOT NULL DEFAULT 0,
    polls            INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    ended_at_ms      INTEGER
);

CREATE INDEX mission_waits_open ON mission_waits(state, due_at_ms);
CREATE INDEX mission_waits_mission ON mission_waits(mission_id, state);

-- A recorded wake. It is delivered in every brief until its item leaves todo/doing.
CREATE TABLE mission_wakes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id      TEXT NOT NULL REFERENCES missions(id),
    wait_id         INTEGER,
    item_key        TEXT NOT NULL,
    reason          TEXT NOT NULL,
    -- Untrusted text from the outside (a mail summary, a webhook body), capped.
    payload_json    TEXT,
    -- A wait wakes once: `wait:<id>`.
    dedupe_key      TEXT NOT NULL UNIQUE,
    created_at_ms   INTEGER NOT NULL,
    consumed_at_ms  INTEGER
);

CREATE INDEX mission_wakes_open ON mission_wakes(mission_id, consumed_at_ms);
