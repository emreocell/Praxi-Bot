-- Signatures already accepted by an incoming webhook, kept until their
-- five-minute window ends so a restart cannot replay a captured request.
CREATE TABLE incoming_webhook_seen (
    webhook_id TEXT NOT NULL REFERENCES incoming_webhooks(id) ON DELETE CASCADE,
    signature  TEXT NOT NULL,
    expires_ms INTEGER NOT NULL,
    PRIMARY KEY (webhook_id, signature)
);
CREATE INDEX idx_incoming_webhook_seen_expiry ON incoming_webhook_seen(expires_ms);
