# state-bundle

Export and import the portable state owned by a Praxi-Bot installation. The
public async functions accept any unpinned `AsyncWrite` / `AsyncRead`. Payloads
are streamed in 64 KiB chunks, with backpressure. SQLite snapshots, migrations,
filesystem traversal and the final installation use synchronous APIs; call from
a dedicated Tokio worker/runtime when latency on the caller's thread matters.

## Format v2

An uncompressed tar stream of regular files, followed by `manifest.json` as the
last entry and two zero end blocks. USTAR paths are UTF-8; export fails rather
than truncating paths that USTAR cannot represent. The importer rejects extension
records (including PAX and GNU long-name records), directory entries, links,
special files, duplicate/case-aliasing paths, traversal, absolute paths, Windows
device names, alternate data streams, and paths outside the allowlist. Additional
zero record padding at EOF is accepted and counted against the stream limit.

The JSON manifest contains `format_version` (2), `schema_version` (SQLite
`user_version` for `bot.db`), `app_version`, `epoch`, `created_at` (Unix seconds),
optional public `source_key`, sorted `secret_names`, and ordered
`files: [{path, size, sha256}]`. SHA-256 is lowercase hexadecimal over each
payload's exact bytes. The manifest lists every payload, including `secrets.json`,
but does not hash itself. Missing, extra, reordered, or altered entries fail
verification. Unsupported format/schema versions fail before target writes. Older
`bot.db` schemas are migrated with `storage::migrate` only after verification;
the returned manifest continues to describe the received snapshot.

## Included state and actual data directory layout

The central constants `INCLUDED_DIRECTORIES`, `INCLUDED_FILES`, and `EXCLUSIONS`
define the format's filesystem policy.

| Relative path | Content |
| --- | --- |
| `bot.db` (required) | Bots, JSON bot profiles, users, chats, messages and runs |
| `policy.json` (required) | Versioned global permission policy, including initiator overrides and limits |
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

## Global permission policy

Every v2 bundle contains a hashed `policy.json` with `schema_version: 1` and the
daemon's existing top-level `categories` object. The installed file is readable
by `daemon-api::load_policy`; migration uses its canonical `validate_policy`
function. Unknown policy fields and unsupported policy versions are rejected
rather than silently dropping constraints. The manifest's SQLite
`schema_version` is independent of this policy schema version.

An existing unversioned local policy remains supported. When the source file is
absent, export records the daemon's `approve_destructive` default explicitly.
Unreadable, malformed, non-regular, and oversized source policies fail export.
The source policy is never rewritten. Import rejects a missing, unversioned,
malformed, or unsupported policy payload, including legacy v1 bundles that did
not reliably include the policy. Re-export legacy state with the current
exporter; `allow_policy_relaxation` does not bypass format or validation errors.

A destination with no `policy.json` is not an absent ceiling. The daemon's
effective policy for a missing file is `approve_destructive` (Ask everywhere),
on a fresh empty target and on an initialized host (`replace: true`). The
incoming policy must preserve that default. A fresh empty target still accepts
the default, or a stricter policy, without `allow_policy_relaxation`.

When a destination policy file exists, import must preserve its permission
ceiling for every category and initiator, including missing-category `Ask`
defaults and schedule-to-human inheritance. Equal levels/limits, a new `Deny`,
or replacing an `Allow` are safe without an override. Other changes, including
any loosening of the missing-file default, require
`ImportOptions::allow_policy_relaxation: true`, which defaults to `false` and
is separate from `replace`. A malformed, unreadable, or non-regular destination
policy is rejected even with that flag; it cannot be treated as a missing
policy or an approval of an unknown ceiling.

To recover a corrupt destination `policy.json` and import again, move that file
aside or repair the JSON, then retry the import. While the daemon is running,
`PUT /v1/policy` with a preset such as `approve-destructive` replaces the file
and clears the deny-all lock; stop the daemon before importing. A failed load
may already have kept a copy named `policy.json.corrupt-<ms>`. Deleting the
file does not approve a looser bundle: a missing file is still the
`approve_destructive` ceiling.

The comparison deliberately does not rank `Ask` below `AllowWithLimits`:
persisted grants in `bot.db` can waive ordinary `Ask`, whereas the runtime keeps
unsupported limits subject to approval. Switching between those levels, or
changing any limit, therefore needs explicit approval even if the caller
expects the change to be stricter. The destination policy is checked again at
the swap boundary, after progress callbacks, before any secret or live state
write. `commit` then captures the policy file actually present at the rename,
including one created or replaced after that check, and rolls the swap back
when the incoming policy would relax it. This crate does not hold the daemon's
policy lock. Stop the target daemon, or otherwise serialize the import with
`PUT /v1/policy`. Database grants are preserved together with the imported
policy, so a source `Deny` does not become the daemon's weaker default on the
destination.

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
is 2 GiB and the default per-entry cap is 1 GiB. Manifest, secrets, and policy entries
have a further 8 MiB cap, and entry count is capped at 100,000. Limits include
tar headers/padding/trailing data. Only manifest/secrets payloads and transfer
chunks are buffered during transfer; ordinary files go straight to staging.
Policy validation separately reads at most 8 MiB, with all handles closed
before replacement or staging cleanup on Windows as well as Unix.

Nonempty targets (bots **or** chats) require `replace: true`. Verification,
SQLite integrity checking and migrations, host config validation, and reading
old secret values all precede target writes. Import rewrites host `mode` and
`state` from options (defaults: `standalone` and `standby`), sets the received
epoch, and removes the source's `leader`. Other host settings are retained.
Explicit activation/epoch advancement is the handover caller's responsibility.

The primary database must also contain the tables and columns used by storage,
with valid foreign-key references. Its run ancestry must be rooted, acyclic and
within the agent runtime's existing subagent depth limit (root 0, maximum 2).
These checks run only on staged imports, before installation; ordinary database
opening is unchanged. Valid older schemas are migrated before these checks.

The install phase has no async cancellation points. Original values are retained
in memory for secret rollback; old portable roots and DB sidecars are renamed
into `<data>/backup-<unix-ts>/`, then staged roots are renamed into place.
The previous policy moves after the old databases and their sidecars; the
incoming policy is installed before the imported databases. Reverse rollback
likewise restores the previous policy before its databases. This ordering keeps
the policy in place whenever the imported grant database becomes live.
Each rename is atomic. Errors trigger reverse renames and secret restoration;
rollback failure produces `ImportError::Recovery` with the retained backup path.
Existing machine identity and `secrets.enc` remain at their original paths.
Absent optional roots in the incoming bundle replace old optional state too.
Backups contain old portable filesystem state, not old secrets or identities.

**Atomicity limitation:** this is a rollback-protected installation, not a
single crash-atomic directory exchange or a transaction spanning SQLite files
and arbitrary secret backends. A process/power failure during installation can
require manual recovery from the backup/staging directories. Secret-store
rollback can itself fail. Do not allow concurrent readers or writers during
swap, including `PUT /v1/policy`. Commit re-checks the policy it moved into the
backup and rolls back when that captured file is tighter than the pre-check
saw, but it does not take the daemon's policy lock.
A crash journal/platform-specific exchange is a follow-up for stronger recovery.

The latest three timestamped backup directories are retained; successive imports
within one second receive increasing timestamp suffixes. Pruning is best effort
after commit so a cleanup failure cannot misreport a successful installation as
failed. Permission or I/O failures may leave extra backups for manual cleanup.

Both options accept a synchronous `ProgressCallback` (`Arc<dyn Fn(Progress) +
Send + Sync>`), reporting cumulative archive bytes and export/receive/verify/
migrate/swap/complete phases. Callbacks must not block or panic.
