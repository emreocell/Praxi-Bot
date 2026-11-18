-- Files (and directory trees) whose content did not come from the user: written by Praxi's own
-- tools while the run held untrusted content, downloaded, cloned, fetched. Kept across chats and
-- restarts so a later read of such a file is classified untrusted.
CREATE TABLE file_provenance (
    path TEXT NOT NULL PRIMARY KEY,
    tree INTEGER NOT NULL DEFAULT 0 CHECK (tree IN (0, 1)),
    origin TEXT NOT NULL DEFAULT '',
    recorded_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
