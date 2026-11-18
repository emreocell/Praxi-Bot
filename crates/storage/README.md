# storage

The storage crate is Praxi-Bot's persistence layer. It keeps users, bots, chats,
messages, runs, run steps, approvals and the audit log in a SQLite database, and
writes each run's events to an append-only NDJSON file so a client can replay a
stream.

## Entry points

- [`Storage`](src/db.rs) is a synchronous repository over SQLite.
  `Storage::open(data_dir)` creates or opens `<data_dir>/bot.db`, enables WAL and
  foreign keys, and runs pending migrations; `Storage::open_in_memory` is for
  tests. Calls serialise on an internal mutex, so wrap them in `spawn_blocking`
  where blocking matters.
- [`records`](src/records.rs) holds the row types (`User`, `Bot`, `Chat`,
  `Message`, `Run`, `RunStep`, `Approval`, `AuditEntry`, ...).
- [`migrate`](src/migrate.rs) runs the embedded, numbered SQL files from
  `migrations/`.
- [`EventLogWriter`, `event_log_path` and `read_event_log`](src/event_log.rs)
  write and read `<data_dir>/runs/<run_id>.ndjson`.
- [`EventJournal`](src/journal.rs) batches event-log writes on a background
  thread; `flush` is the barrier that returns once earlier records are durable.
- [`paths`](src/paths.rs) resolves the data directory (`DATA_DIR_ENV`).

## Invariants

`audit_log` is append-only. The API has no update or delete for it, and the
`audit_log_no_update` and `audit_log_no_delete` triggers in
`migrations/0001_init.sql` abort any attempt at the SQL level. Migrations are
also forward-only: never edit one that has shipped, add a new numbered file and
register it in `src/migrate.rs`.

## Tests

From the workspace root, run `cargo test -p storage`. Most tests use an in-memory
database or a temporary directory.
