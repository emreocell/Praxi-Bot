-- Incoming webhooks: an owner-created address that starts a bot run.
-- The signing secret stays in this table and is never copied into the audit log.
CREATE TABLE incoming_webhooks (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES users(id),
    bot_id           TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    instruction      TEXT NOT NULL,
    secret           TEXT NOT NULL,
    enabled          INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    created_at_ms    INTEGER NOT NULL,
    updated_at_ms    INTEGER NOT NULL,
    last_fired_at_ms INTEGER
);
CREATE INDEX idx_incoming_webhooks_bot ON incoming_webhooks(bot_id, created_at_ms);
