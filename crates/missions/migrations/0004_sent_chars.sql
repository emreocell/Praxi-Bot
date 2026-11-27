-- What the mission's sessions have sent, in characters, to places the owner did not name
-- (URLs, search queries, form text). One total for the whole mission, kept here and not in a
-- session, so a fresh session does not start from zero. Written only by the daemon.

ALTER TABLE missions ADD COLUMN sent_chars INTEGER NOT NULL DEFAULT 0;
