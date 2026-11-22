# Encrypted backup and restore

`bot backup` writes your bots, chats, memory, skills, workflows, schedules and provider
settings into **one passphrase-encrypted file**. It is safe to keep on a cloud drive or a
USB stick, and you can restore it on the same or another machine. Use it before every
update and when you move to a new computer.

```sh
bot backup create ./praxi.bak          # asks for a passphrase (hidden input)
bot backup verify ./praxi.bak          # decrypts and checks everything; changes nothing
bot backup restore ./praxi.bak         # stop the daemon first
```

## The passphrase

The passphrase is never accepted as a command-line value (it would show up in the process
list and your shell history). Choose one way:

| How | Use |
| --- | --- |
| Hidden prompt (default) | `bot backup create FILE` asks twice when creating, once otherwise |
| File | `--passphrase-file ./pass.txt` (first line; keep the file private) |
| Environment variable | `--passphrase-env MY_BACKUP_PASS` (the **name** of the variable) |

A new passphrase needs at least 8 characters. **There is no recovery:** without the
passphrase a backup cannot be opened.

## What is inside

Bots, chats and history, memory, skills, workflows, schedules, the permission policy and
provider/MCP settings. Always left out: the API token, the peer identity key, paired
devices, browser profiles, logs and earlier `backup-*` folders. After restoring on a new
machine, pair devices again and use the new API token.

### API keys (`--include-secrets`)

Keys are **not** in a backup unless you pass `--include-secrets` to `create`. They are
stored encrypted like everything else. Even then, `restore` writes them into your secret
store only if you also pass `--include-secrets` to `restore`; otherwise they are skipped
and the command says how many were skipped.

## Create

`create` never overwrites an existing file, writes to a `.partial` file first and renames it
when complete, and is safe while the daemon runs (SQLite files are copied with the online
backup API). A backup taken with the daemon stopped is consistent across all files. Large
data directories are streamed; they do not have to fit in memory.

## Verify

`verify` decrypts the whole file and compares every file with the manifest. It reports the
file count, size, Praxi version and whether secrets are included. It never touches the
data directory and does not write decrypted data to disk.

## Restore

`restore` refuses to run while a daemon holds the data directory (a `.daemon.lock` file held
by `bot serve`) or answers on the service port. It decrypts and checks the entire file
**before** touching anything, so a wrong passphrase or a damaged file never changes your
data. If the installation already has bots or chats you must pass `--replace`; the state it
replaces is kept in a `backup-*` folder inside the data directory. Use
`--allow-policy-relaxation` to accept a backup whose permission policy is more permissive
than the current one.

Do not modify the backup file while a restore runs.

## Damaged files and wrong passphrases

A wrong passphrase, a flipped byte, a truncated file and reordered parts all give the same
message: *wrong passphrase, or the backup file is damaged or incomplete (nothing was
changed)*. Authentication covers the header and every part of the file, so nothing is
trusted before it is verified.

## Format (for the curious)

Magic `PRAXIBAK`, version, Argon2id parameters (64 MiB, 3 passes by default) and a random
salt in the header; then 64 KiB chunks sealed with ChaCha20-Poly1305. The header is
authenticated data for every chunk, the nonce encodes the chunk number and a final-chunk
flag, so reordering, dropping, truncating or appending is detected. The payload is the
`state-bundle` stream.

## Older plaintext backups

Versions before encrypted backups wrote unencrypted `.tar` files with `bot backup FILE`.
`bot restore FILE` still restores those; see [Backup and restore](backup-and-restore.md).
Create new backups with `bot backup create`.

## See also

- [CLI reference](cli-reference.md#bot-backup-create--verify--restore)
- [Host operations](host-operations.md#backup-and-restore)
