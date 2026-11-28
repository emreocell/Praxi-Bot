-- A deleted bot whose cleanup outside this database (files, indexes, memory, links, mailboxes)
-- has not finished. Written in the same transaction that deletes the bot, removed once the
-- cleanup is done, so a crash or a failing step leaves something to retry. No foreign key: the
-- bot row is gone.
CREATE TABLE bot_delete_cleanup (
    bot_id        TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    name          TEXT NOT NULL,
    -- `{"run_ids": [...], "chat_ids": [...], "webhook_ids": [...]}`: what the files and secrets
    -- left behind are named after.
    detail_json   TEXT NOT NULL,
    -- The owner asked to delete the bot's own AgentMail inbox at the provider as well.
    delete_inbox  INTEGER NOT NULL CHECK (delete_inbox IN (0, 1)),
    created_at_ms INTEGER NOT NULL
);
