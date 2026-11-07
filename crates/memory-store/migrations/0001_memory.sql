-- Memory store schema (own database file `memory.db`, own user_version).
-- ids of users/bots/chats are plain TEXT (UUIDs): they live in bot.db, so no FKs.
-- The vector table `memories_vec` (vec0) is created at open time because its
-- dimension depends on the configured embedder (recorded in `meta`).

CREATE TABLE memories (
    rowid             INTEGER PRIMARY KEY AUTOINCREMENT,
    id                TEXT NOT NULL UNIQUE,
    user_id           TEXT NOT NULL,
    bot_id            TEXT,
    kind              TEXT NOT NULL CHECK (kind IN ('profile','bot','episodic','procedural','lesson')),
    content           TEXT NOT NULL,
    source_quote      TEXT,
    source_chat_id    TEXT,
    source_message_id TEXT,
    trust             TEXT NOT NULL CHECK (trust IN ('user','untrusted')),
    importance        REAL NOT NULL DEFAULT 0.5,
    created_ms        INTEGER NOT NULL,
    last_used_ms      INTEGER NOT NULL,
    valid_until_ms    INTEGER,
    deleted           INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_memories_scope ON memories(user_id, bot_id, deleted);

-- Lexical index; rowid = memories.rowid, maintained by the store.
CREATE VIRTUAL TABLE memories_fts USING fts5(content, tokenize = 'unicode61 remove_diacritics 2');

-- Every add / update / delete with full before/after snapshots (JSON), for undo.
CREATE TABLE memory_journal (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id   TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    op          TEXT NOT NULL CHECK (op IN ('add','update','delete')),
    before_json TEXT,
    after_json  TEXT,
    reason      TEXT,
    ts_ms       INTEGER NOT NULL,
    undone      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_journal_user ON memory_journal(user_id, id);

-- Cross-session message search (fed from bot.db messages by the caller).
CREATE TABLE session_msgs (
    rowid      INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL UNIQUE,
    user_id    TEXT NOT NULL,
    chat_id    TEXT NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE INDEX idx_session_user ON session_msgs(user_id);
CREATE VIRTUAL TABLE session_fts USING fts5(content, tokenize = 'unicode61 remove_diacritics 2');

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
