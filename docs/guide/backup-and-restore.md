# Restoring older plaintext backups

Current versions create **encrypted** backups; see [Encrypted backup and restore](backup.md).
Older versions wrote unencrypted archives with `bot backup <FILE>`. `bot restore` still reads
those:

```sh
bot restore ./praxi-backup.tar            # stop the daemon first
bot restore ./praxi-backup.tar --replace  # swap existing bots and chats
```

- It refuses to run while a daemon holds the data directory or answers on the service port.
- An installation that already has bots or chats needs `--replace`; the replaced state is kept
  in a `backup-*` folder inside the data directory.
- `--allow-policy-relaxation` accepts a backup whose permission policy is more permissive
  than the current one.
- Plaintext archives contain no API keys: enter them again (`bot secrets set`, `bot setup`).
- With `--json`, one JSON object is printed on success: `file`, `files`, `app_version`,
  `previous_state`. Errors go to stderr as text.

Treat old `.tar` backups as sensitive and consider re-creating them with `bot backup create`.
