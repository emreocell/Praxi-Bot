# state-bundle

Export and import the portable state owned by a Praxi-Bot installation. The
public async functions accept any unpinned `AsyncWrite` / `AsyncRead`. Payloads
are streamed in 64 KiB chunks, with backpressure. SQLite snapshots, migrations,
filesystem traversal and the final installation use synchronous APIs; call from
a dedicated Tokio worker/runtime when latency on the caller's thread matters.

## Format v1

An uncompressed tar stream of regular files, followed by `manifest.json` as the
last entry and two zero end blocks. USTAR paths are UTF-8; export fails rather
than truncating paths that USTAR cannot represent. The importer rejects extension
records (including PAX and GNU long-name records), directory entries, links,
special files, duplicate/case-aliasing paths, traversal, absolute paths, Windows
device names, alternate data streams, and paths outside the allowlist. Additional
zero record padding at EOF is accepted and counted against the stream limit.

The JSON manifest contains `format_version` (1), `schema_version` (SQLite
`user_version` for `bot.db`), `app_version`, `epoch`, `created_at` (Unix seconds),
optional public `source_key`, sorted `secret_names`, and ordered
`files: [{path, size, sha256}]`. SHA-256 is lowercase hexadecimal over each
payload's exact bytes. The manifest lists every payload, including `secrets.json`,
but does not hash itself. Missing, extra, reordered, or altered entries fail
verification. Newer format/schema versions fail before target writes. Older
`bot.db` schemas are migrated with `storage::migrate` only after verification;
the returned manifest continues to describe the received snapshot.

## Included state and actual data directory layout

The central constants `INCLUDED_DIRECTORIES`, `INCLUDED_FILES`, and `EXCLUSIONS`
define the format's filesystem policy.

| Relative path | Content |
| --- | --- |
| `bot.db` (required) | Bots, JSON bot profiles, users, chats, messages and runs |
| `memory.db` (optional) | Semantic memory and indexes, stored separately by memory-store |
| `schedules.db` (optional) | Schedules and triggers; the current scheduler uses a separate DB |
| `bots/`, `profiles/` | Optional file-based bot profiles |
| `skills/` | User skills, including nested `SKILL.md` files |
| `workflows/` | Saved TOML workflows and their index |
| `providers.toml` | Provider configuration and secret references |
| `mcp.toml` | Optional MCP configuration |
| `host.toml` | Host metadata, rewritten on import |

All SQLite files use the online backup API into private temporary files,
including committed WAL data. Live database/WAL/SHM files are never copied as
individual archive entries. Quiesce the source for a snapshot consistent across
the databases, filesystem state and secret store.

Always excluded, case insensitively at every depth: `api-token`, `identity.key`,
`devices.json`, `peer-node/`, `secrets.enc`, `browsers/`, `browser/`,
`browser-profiles/`, `browser_profiles/`, `logs/`, `backup-*`, `import-staging/`.
All other non-allowlisted roots are excluded too, including run event logs.
Unknown and machine-local target roots are preserved during replacement.

## Secrets and trust

`SecretSelection` defaults to `None`; `All` and `Names(Vec<String>)` are explicit
choices. Selected values are encoded as a JSON object in `secrets.json`. That
entry is never written to disk by either exporter or importer. Owned plaintext
JSON, transfer buffers, imported strings and rollback copies use `Zeroizing`;
values fetched from a store use its redacted `SecretValue`. The existing
secret-store type and backend internals do not guarantee zeroization. This crate
cannot eliminate their copies, allocator reallocations, OS paging or crash dumps.
Stores may persist encrypted values as part of their normal `set` operation.

Bundles contain plaintext secrets and potentially sensitive provider/MCP config.
Use an authenticated encrypted channel (the handover API will use iroh), or
encrypt saved bundles. Hashes detect corruption, not a malicious sender who can
rewrite the manifest. `source_key` is informational, not a signature. Do not log
payloads. Only trusted owners should have write access to the data directory;
filesystem mutation by concurrent local processes is outside this API's model.

## Import, replacement and recovery

Stop the target daemon, close **all** target SQLite handles and serialize imports.
The fixed `<data>/import-staging/` directory is created exclusively, with mode
0700 on Unix; an existing staging path is refused. The owned staging directory
is removed on ordinary failure or async cancellation. The default archive cap
is 2 GiB and the default per-entry cap is 1 GiB. Manifest and secrets entries
have a further 8 MiB cap, and entry count is capped at 100,000. Limits include
tar headers/padding/trailing data. Only manifest/secrets payloads and transfer
chunks are buffered; ordinary files go straight to staging.

Nonempty targets (bots **or** chats) require `replace: true`. Verification,
SQLite integrity checking and migrations, host config validation, and reading
old secret values all precede target writes. Import rewrites host `mode` and
`state` from options (defaults: `standalone` and `standby`), sets the received
epoch, and removes the source's `leader`. Other host settings are retained.
Explicit activation/epoch advancement is the handover caller's responsibility.

The install phase has no async cancellation points. Original values are retained
in memory for secret rollback; old portable roots and DB sidecars are renamed
into `<data>/backup-<unix-ts>/`, then staged roots are renamed into place.
Each rename is atomic. Errors trigger reverse renames and secret restoration;
rollback failure produces `ImportError::Recovery` with the retained backup path.
Existing machine identity and `secrets.enc` remain at their original paths.
Absent optional roots in the incoming bundle replace old optional state too.
Backups contain old portable filesystem state, not old secrets or identities.

**Atomicity limitation:** this is a rollback-protected installation, not a
single crash-atomic directory exchange or a transaction spanning SQLite files
and arbitrary secret backends. A process/power failure during installation can
require manual recovery from the backup/staging directories. Secret-store
rollback can itself fail. Do not allow concurrent readers/writers during swap.
A crash journal/platform-specific exchange is a follow-up for stronger recovery.

The latest three timestamped backup directories are retained; successive imports
within one second receive increasing timestamp suffixes. Pruning is best effort
after commit so a cleanup failure cannot misreport a successful installation as
failed. Permission or I/O failures may leave extra backups for manual cleanup.

Both options accept a synchronous `ProgressCallback` (`Arc<dyn Fn(Progress) +
Send + Sync>`), reporting cumulative archive bytes and export/receive/verify/
migrate/swap/complete phases. Callbacks must not block or panic.
