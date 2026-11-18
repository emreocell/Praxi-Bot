-- Folders the owner told Praxi to treat as their own content (a git work tree with a remote is
-- otherwise "outside content"). Files a bot fetched there stay untrusted through the provenance
-- ledger. Local only; changed through owner-key routes.
CREATE TABLE trusted_folders (
    path TEXT NOT NULL PRIMARY KEY,
    added_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
