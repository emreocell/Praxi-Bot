-- Corrections that hold, an undo that leaves a row, and a real erase.
--
-- * `memory_tombstones`: one row per forgotten statement.  It keeps a
--   fingerprint of the normalised text, a token-hash set and the embedding (no
--   readable text except a short label that is dropped on permanent erase), so
--   the extraction pipeline can refuse to learn the same thing again.
-- * `memory_journal` is rebuilt because SQLite cannot widen a CHECK: the new
--   ops `undo` and `purge` and the `redacted` marker are added.

CREATE TABLE memory_tombstones (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    bot_id      TEXT,
    memory_id   TEXT,
    kind        TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    tokens      BLOB NOT NULL,
    embedding   BLOB NOT NULL,
    label       TEXT,
    erased      INTEGER NOT NULL DEFAULT 0,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX idx_tombstones_scope ON memory_tombstones(user_id, bot_id);
CREATE UNIQUE INDEX idx_tombstones_fp ON memory_tombstones(user_id, IFNULL(bot_id, ''), fingerprint);

CREATE TABLE memory_journal_v2 (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id   TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    op          TEXT NOT NULL CHECK (op IN ('add','update','delete','undo','purge')),
    before_json TEXT,
    after_json  TEXT,
    reason      TEXT,
    ts_ms       INTEGER NOT NULL,
    undone      INTEGER NOT NULL DEFAULT 0,
    redacted    INTEGER NOT NULL DEFAULT 0
);
INSERT INTO memory_journal_v2 (id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone)
    SELECT id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone FROM memory_journal;
DROP TABLE memory_journal;
ALTER TABLE memory_journal_v2 RENAME TO memory_journal;
CREATE INDEX idx_journal_user ON memory_journal(user_id, id);
CREATE INDEX idx_journal_memory ON memory_journal(memory_id);
