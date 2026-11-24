# Memory: corrections that hold, and a real erase (ADR)

Status: accepted. Scope: `crates/memory-store`, `crates/agent-core/src/memory.rs`,
`crates/daemon-api/src/memory.rs`, the "Personal memory" card in the desktop app.

An independent reading of the memory subsystem (an LLM review of commit
`8e51eaf1`, a reading, not a run) listed several weaknesses. Each was checked
against the code first. All but one were confirmed; the outcome per finding is
below.

## Findings and decisions

| # | Finding | Verified | Decision |
|---|---------|----------|----------|
| 1 | A forgotten memory is soft-deleted and removed from the indexes, so the next extraction that meets the same statement adds it again | yes | **Tombstones** (`memory_tombstones`, migration 0002) |
| 2 | "Delete" keeps the text (row, journal snapshots, exports) | yes | **`purge`** ("Erase permanently"); exports skip forgotten rows |
| 3 | Undo only flags the old journal entry, leaving no row for the undo itself | yes | Undo **appends** a journal entry (`op = undo`) in the same transaction |
| 4 | `memory_forget` with a query deleted the top recall hit blindly | yes | A query only **lists candidates**; deleting needs the `memory_id` |
| 5 | The extraction pipeline accepted a quote found only in the assistant part of the excerpt | yes | Quote must be a span of a **user** message (`RejectReason::QuoteNotFromUser`) |
| 6 | Deleting a chat or a bot leaves its text in `session_msgs`/`session_fts`, and bot-scoped memories | yes | `erase_chat`, `erase_bot`, called from the delete routes |
| 7 | `secrets.rs` refuses commit hashes (40 hex / 64 hex) | yes | Bare hex of hash length is allowed **only** after a naming word |

Nothing in the review was found wrong. One point it could not know: the
owner-only routes and untrusted-content rules already covered the write paths,
so no security rule had to change; the new routes are owner-gated the same way.

## Tombstones (finding 1)

* Forgetting (owner panel, `memory_forget`, or a retraction the pipeline
  recognises) writes a tombstone in the same transaction as the soft delete.
* A tombstone is scoped like the memory (user, and bot when bot-scoped; a shared
  memory's tombstone applies to every bot of the user).
* It stores: a SHA-256 fingerprint of the normalised words (case, punctuation
  and spacing ignored), a sorted set of 8-byte word hashes, and the embedding.
  The only readable part is a 80-character label for the owner's "Forgotten
  items" list; it is dropped when the item is erased permanently. No tombstone
  exists without the owner having forgotten something.
* `Pipeline::handle` asks `MemoryStore::is_forgotten` before the similarity
  search and refuses with `RejectReason::Forgotten`. A candidate matches when
  the fingerprint is equal, or when cosine >= 0.85 **and** word-set Jaccard
  >= 0.7. The second condition matters: embedding models score "lives in Paris"
  and "lives in Berlin" very high, and a plain cosine would block a legitimate
  move.
* The owner can always re-add: `MemoryStore::add` lifts the tombstone of the
  same statement; undoing a forget lifts it; the panel lists tombstones and can
  remove one ("Allow learning again"). `DELETE /v1/memory/forgotten/{id}`.
* Trade-off: an embedding and word hashes are derived data. They are not text,
  but a determined attacker with the database could test guesses against the
  hashes. The alternative (keeping no fingerprint) means the correction does
  not hold, which is the bug.

## Permanent erase (finding 2)

`MemoryStore::purge(user, id, reason)` (`POST /v1/memory/{id}/erase`, owner key
required) in one transaction:

1. writes / upgrades the tombstone to text-free (`erased = 1`, no label);
2. removes the FTS and vector entries;
3. replaces `before_json`/`after_json` of every journal entry of the memory
   with NULL and sets `redacted = 1` (operation, time and reason stay);
4. deletes the `memories` row and appends a `purge` journal entry;
5. merges the FTS segments, and after the commit checkpoints the WAL.

The connection runs with `PRAGMA secure_delete = ON`, so freed pages are
overwritten.

Exports: `export_json` leaves out forgotten rows **and** the journal entries of
memories that are not live (their snapshots carry the text).
`export_json_with(user, true)` is the explicit full local backup.

### What erase does not do

* **Backups made earlier still contain the text.** Erase cannot reach them.
* **Restoring a backup (`restore --replace`) can resurrect forgotten or erased
  items**, because the backup predates the tombstone. After a restore the owner
  should open "Forgotten items" and erase again what must stay gone; the
  tombstones restored with the database keep blocking the statements the
  backup knew about, but items forgotten after the backup are back.
  (A "re-apply tombstones after restore" helper is not implemented; the panel
  actions above are the supported way.)
* Memories the owner never forgot but that were *derived* from an erased chat
  keep their text; they are the owner's to forget.

## Undo is recorded (finding 3)

`undo` writes a journal entry `op = undo` with `before` = state before the undo,
`after` = restored state, reason `undo of journal entry N`, in the same
transaction that flags the old entry. The new entry is created already
"undone" so the undo stack keeps its shape: the next undo still reaches the
entry below. Undoing a forget also lifts the tombstone.

## Forget by query (finding 4)

`memory_forget` with a `query` no longer deletes. It returns up to five
candidates (id, kind, text inside an escaped data block, marked untrusted when a
candidate is untrusted) and says nothing was forgotten. Deleting takes the
`memory_id`. The id is scoped to the calling bot's visible memories.

## Quote rule (finding 5)

`ExtractionContext.user_messages` carries the user-authored messages of the
excerpt (redacted like the excerpt). A quote must be a whitespace/case-
normalised span of one of them, like the `memory_save` tool. A quote found only
in the assistant part is rejected as `QuoteNotFromUser`.

## Chat and bot deletion (finding 6)

* `DELETE /v1/chats/{id}` calls `MemoryStore::erase_chat` after the chat is
  deleted: its `session_msgs` rows and FTS entries go.
* Deleting a bot (`remove_bot`, shared by the route and the bot tool) calls
  `MemoryStore::erase_bot` inside the cleanup that runs in the delete
  transaction: the bot-scoped memories with their journal and the bot's
  tombstones are removed; memories shared across the user's bots stay. A failure
  rolls the bot deletion back. The confirmation text now says so.
* Rows of chats deleted before this change stay in `session_msgs` until
  re-indexing; not handled here.

## Secrets (finding 7)

The 40+ char base64 and 32+ char hex patterns stay. A bare hex run of exactly
32, 40 or 64 characters is allowed when the 48 characters before it end with a
naming word (`commit`, `sha`, `sha256`, `md5`, `checksum`, `hash`, `digest`,
`revision`, `git checkout|show|...`) and no credential word (`secret`,
`password`, `token`, `api key`, `private`, ...) is in that window. Everything
else, and every provider key pattern, is unchanged.

## Not done

* A tombstone-reapply step after `restore --replace` (see above).
* Pruning `session_msgs` of chats that were deleted before this change.
