CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE bots (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id),
    name          TEXT NOT NULL,
    config_json   TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_bots_user ON bots(user_id);

CREATE TABLE chats (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id),
    bot_id        TEXT NOT NULL REFERENCES bots(id),
    title         TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_chats_user ON chats(user_id);

CREATE TABLE runs (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id),
    bot_id        TEXT NOT NULL REFERENCES bots(id),
    chat_id       TEXT REFERENCES chats(id),
    kind          TEXT NOT NULL,
    state         TEXT NOT NULL,
    error         TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_runs_user ON runs(user_id);
CREATE INDEX idx_runs_state ON runs(state);

CREATE TABLE messages (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id),
    chat_id       TEXT NOT NULL REFERENCES chats(id),
    run_id        TEXT REFERENCES runs(id),
    role          TEXT NOT NULL,
    content       TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_messages_chat ON messages(chat_id, created_at_ms);

CREATE TABLE run_steps (
    id             TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL REFERENCES runs(id),
    user_id        TEXT NOT NULL REFERENCES users(id),
    idx            INTEGER NOT NULL,
    name           TEXT NOT NULL,
    input_json     TEXT NOT NULL,
    idempotent     INTEGER NOT NULL,
    retry_json     TEXT,
    status         TEXT NOT NULL,
    attempt        INTEGER NOT NULL,
    output_json    TEXT,
    error          TEXT,
    started_at_ms  INTEGER NOT NULL,
    finished_at_ms INTEGER,
    UNIQUE (run_id, idx)
);

CREATE TABLE approvals (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users(id),
    run_id          TEXT NOT NULL REFERENCES runs(id),
    step_id         TEXT REFERENCES run_steps(id),
    summary         TEXT NOT NULL,
    decision        TEXT,
    requested_at_ms INTEGER NOT NULL,
    decided_at_ms   INTEGER
);
CREATE INDEX idx_approvals_run ON approvals(run_id);

-- Append-only (ADR-0012 floor): enforced at the schema level as well as in the API.
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT NOT NULL REFERENCES users(id),
    run_id      TEXT,
    actor       TEXT NOT NULL,
    action      TEXT NOT NULL,
    detail_json TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL
);
CREATE INDEX idx_audit_user ON audit_log(user_id, ts_ms);

CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;

CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;
