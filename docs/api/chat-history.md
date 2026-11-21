# Chat history search and export (version 1)

These are explicit, local owner actions: send the bearer token and
`x-praxi-owner-key`. They do not add network access or grant paired peers new
authority. Responses contain conversation data; exports have `Cache-Control: no-store`.

`GET /v1/chats/search?q=phrase&bot_id=UUID&chat_id=UUID&offset=0&limit=30`
searches a literal phrase across the owner's chats, including archived chats.
Both scope filters are optional and intersect when supplied. Results contain
chat/bot/message IDs, redacted chat title, Unix millisecond time and a plain-text
snippet (at most 320 characters). `next_offset` continues the ranked page; no total
count is exposed. Limit is 1–100, offset 0–10,000, query 1–200 characters.
Pages can change as messages arrive or are deleted.

Storage migration 0019 uses bundled SQLite FTS5 BM25 ranking with
`unicode61 remove_diacritics 2`, supplemented by Turkish dotless-i folding. It
backfills existing messages once, then maintains one FTS row per eligible message
with insert/update/delete triggers. It indexes user/assistant text only, strips
attachment markers, and excludes private, superseded and >64 KiB messages.
Tool output is never indexed. Indexing is synchronous, proportional to one bounded
message, with no provider call or asynchronous indexing queue. The index is local
storage and has the same filesystem protection as the messages table. At read time
the whole message passes the current credential redactor; changed messages and
redacted stubs are omitted from search, so a secret-only match yields no snippet.

`GET /v1/chats/UUID/export?format=markdown` downloads a UTF-8 Markdown document;
`format=json` downloads JSON. The filename is `chat-UUID.md` or `chat-UUID.json`.
JSON conforms to [chat-export.schema.json](chat-export.schema.json); `format_version`
is `1`. IDs are UUID strings; times are UTC Unix milliseconds. Messages are ordered
oldest first, steps and approvals each by time. Markdown renders UTC ISO timestamps.
The desktop chat menu provides both save actions; `/export` saves Markdown.

Exports contain current user/assistant messages, attachment **display names only**,
one line per tool call (name, step status, start time, run ID), and approval outcomes
with decision time (request time while pending). Tool arguments, output, errors,
approval summaries, attachment paths/URLs/content and superseded turns are omitted.
Known credentials and common credential patterns are redacted in all text fields.
Private messages become `[REDACTED]` with no attachment names. They are never silently
included as plaintext, even during an explicit export.

Oversized exports return `400 bad_request`, without partial files: at most 1,000
messages, 2 MiB of stored message content, 2,000 tool calls, 2,000 approvals and
4 MiB of serialized output. Two concurrent history reads are allowed; excess reads
receive `503 unavailable`. SQLite and file reads run on blocking workers. The API
does not export a run separately; the chat document includes run IDs for correlation.
