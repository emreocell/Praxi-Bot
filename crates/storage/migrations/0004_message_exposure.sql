-- Provenance of a chat message: whether the model context that produced it
-- (or that it carries into a later turn) held untrusted or private content,
-- and the source labels the approval card should name. Bits are recorded by
-- the daemon, not inferred from the message text.
ALTER TABLE messages ADD COLUMN exposure_untrusted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN exposure_private INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN exposure_sources TEXT NOT NULL DEFAULT '[]';
