-- Missions wave 3: the ids of the out-of-office notices a mail wait was already put off for
-- (a JSON list, a few entries at most), so the same notice is never counted twice, however its
-- untrusted `Date` header reads. Forward-only.
ALTER TABLE mission_waits ADD COLUMN deferred_ids TEXT NOT NULL DEFAULT '[]';
