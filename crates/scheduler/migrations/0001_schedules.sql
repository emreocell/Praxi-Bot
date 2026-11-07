CREATE TABLE schedules (
    id             TEXT PRIMARY KEY,
    bot_id         TEXT NOT NULL,
    chat_id        TEXT,
    name           TEXT NOT NULL,
    spec_json      TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    enabled        INTEGER NOT NULL DEFAULT 1,
    created_at_ms  INTEGER NOT NULL,
    last_run_at_ms INTEGER,
    next_run_at_ms INTEGER,
    misfire_policy TEXT NOT NULL DEFAULT 'skip'
);

CREATE INDEX idx_schedules_bot ON schedules(bot_id);
CREATE INDEX idx_schedules_next_run ON schedules(enabled, next_run_at_ms);

CREATE TABLE schedule_triggers (
    schedule_id   TEXT PRIMARY KEY REFERENCES schedules(id) ON DELETE CASCADE,
    trigger_json  TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
