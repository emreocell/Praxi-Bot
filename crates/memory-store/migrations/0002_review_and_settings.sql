-- Human review: a memory stored from untrusted-origin content (or any memory
-- while the bot is set to "Ask before saving") is held until the owner approves
-- it.  A held row is indexed but is never returned by recall or the memory tools.
ALTER TABLE memories ADD COLUMN pending INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_memories_pending ON memories(user_id, pending, deleted);
