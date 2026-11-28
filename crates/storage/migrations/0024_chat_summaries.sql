-- Rolling summary of the older part of a long chat (one row per chat). The summarised messages
-- stay in `messages` (searchable, exportable, untouched); this row only replaces them in the
-- model context. `covered_until_*` is the (created_at_ms, id) cursor of the newest summarised
-- message: context = this summary + the messages after the cursor. The row goes with its chat
-- (and its owner): an older binary never sees it, it refuses a newer schema (`SchemaTooNew`).
CREATE TABLE chat_summaries (
    chat_id            TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    user_id            TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    content            TEXT NOT NULL,
    covered_until_ms   INTEGER NOT NULL,
    covered_until_id   TEXT NOT NULL,
    -- Messages summarised so far (all rounds) and how many rounds folded into `content`.
    covered_messages   INTEGER NOT NULL,
    rounds             INTEGER NOT NULL,
    first_message_ms   INTEGER NOT NULL,
    -- What the summarised messages carried (union over every round): the summary inherits it.
    exposure_untrusted INTEGER NOT NULL DEFAULT 0,
    exposure_private   INTEGER NOT NULL DEFAULT 0,
    exposure_sources   TEXT NOT NULL DEFAULT '[]',
    model              TEXT NOT NULL,
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
);
