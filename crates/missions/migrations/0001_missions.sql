-- Missions wave 1: the mission record, its progress ledger, notes, sessions and the timeline.
-- Own database file (`missions.db`), forward-only, like the scheduler store.

CREATE TABLE missions (
    id                TEXT PRIMARY KEY,
    user_id           TEXT NOT NULL,
    bot_id            TEXT NOT NULL,
    chat_id           TEXT NOT NULL,
    title             TEXT NOT NULL,
    -- The owner's words, verbatim. Never rewritten by the model.
    goal_text         TEXT NOT NULL,
    -- Success criterion checked by code: at least this many items `done`.
    -- NULL means every item has to be finished (done, skipped or failed).
    target_done       INTEGER,
    budgets_json      TEXT NOT NULL,
    state             TEXT NOT NULL,
    state_reason      TEXT,
    sessions          INTEGER NOT NULL DEFAULT 0,
    model_steps       INTEGER NOT NULL DEFAULT 0,
    cost_micro_usd    INTEGER NOT NULL DEFAULT 0,
    stalled_sessions  INTEGER NOT NULL DEFAULT 0,
    failed_sessions   INTEGER NOT NULL DEFAULT 0,
    -- Bumped by every ledger write; a session that leaves it unchanged made no progress.
    ledger_version    INTEGER NOT NULL DEFAULT 0,
    questions_asked   INTEGER NOT NULL DEFAULT 0,
    lease_run_id      TEXT,
    -- Owner digest written when the mission ends or blocks (JSON).
    digest_json       TEXT,
    created_at_ms     INTEGER NOT NULL,
    started_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    finished_at_ms    INTEGER
);

CREATE INDEX missions_user_state ON missions(user_id, state);
CREATE INDEX missions_chat ON missions(chat_id);

CREATE TABLE mission_items (
    mission_id     TEXT NOT NULL REFERENCES missions(id),
    key            TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    title          TEXT NOT NULL,
    status         TEXT NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0,
    payload_json   TEXT,
    result         TEXT,
    last_error     TEXT,
    blocked_reason TEXT,
    owner_answer   TEXT,
    -- Parked for a step only the owner can do live (a 2FA / verification code): no typed answer.
    live_only      INTEGER NOT NULL DEFAULT 0,
    updated_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (mission_id, key)
);

CREATE INDEX mission_items_status ON mission_items(mission_id, status, seq);

CREATE TABLE mission_notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id  TEXT NOT NULL REFERENCES missions(id),
    kind        TEXT NOT NULL,
    text        TEXT NOT NULL,
    pinned      INTEGER NOT NULL DEFAULT 0,
    at_ms       INTEGER NOT NULL
);

CREATE INDEX mission_notes_mission ON mission_notes(mission_id, pinned);

CREATE TABLE mission_sessions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id    TEXT NOT NULL REFERENCES missions(id),
    run_id        TEXT,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms   INTEGER,
    outcome       TEXT,
    model_steps   INTEGER NOT NULL DEFAULT 0,
    cost_micro_usd INTEGER NOT NULL DEFAULT 0,
    progressed    INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX mission_sessions_mission ON mission_sessions(mission_id, id);

CREATE TABLE mission_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id  TEXT NOT NULL REFERENCES missions(id),
    at_ms       INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    item_key    TEXT,
    data_json   TEXT
);

CREATE INDEX mission_events_mission ON mission_events(mission_id, id);
