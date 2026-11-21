# Backup and restore

Praxi keeps your bots, chats, memory, skills, workflows, schedules, and provider settings in a local data directory. The `bot backup` and `bot restore` commands package this portable state into a single archive file so you can move to a new computer, create a snapshot before upgrades, or recover from data loss.

## What a backup contains

A backup archive includes all portable application state:

- **Bots**: Profiles, personas, system instructions, and tool configurations.
- **Chats and history**: Conversation threads, message logs, and tool call history.
- **Memory**: Stored bot memories and associations.
- **Skills**: Playbooks and instructions taught to your bots.
- **Workflows**: Saved and recorded tool call sequences.
- **Schedules**: Timed cron triggers, interval schedules, and prompt definitions.
- **Provider settings**: Configured provider models, endpoints, and options.

## What it does not contain

For security, backups **never** contain sensitive credentials or host-specific runtime state:

- **API keys and tokens**: Provider secrets (Gemini, Anthropic, OpenAI keys) and daemon API authentication tokens are excluded. Re-enter these keys after completing a restore (`bot secrets set <provider>`, `bot setup`, or through the desktop app settings).
- **Paired device state**: The device list, the peer identity key and the peer-node state are left out, so remote hosts and paired devices must be paired again.
- **Browser profiles**: The browser profile folders are left out.
- **Logs and earlier backups**: Log files, `backup-*` folders and import staging files are not part of a backup.

> [!NOTE]
> Encrypted backups are a separate feature request ([#197](https://github.com/emreocell/Praxi-Bot/issues/197)). The current commands create unencrypted archives, so store backup files securely.

## Creating a backup

Run `bot backup <FILE>` with the target destination file:

```sh
bot backup ./praxi-backup.tar
```

Output (the file count and size depend on your data):

```text
Backup written to ./praxi-backup.tar (5 files, 320256 bytes).
It does not contain API keys or tokens; enter them again after a restore.
```

Key characteristics:

- **Never overwrites**: If the target file already exists, the command aborts (`backup file <FILE> already exists; choose a new path (existing files are never overwritten)`).
- **Safe while running**: SQLite databases are copied using SQLite's online backup API, making it safe to take backups while the app or daemon runs. Backing up with the app or daemon closed produces a snapshot that is completely consistent across all files.
- **Atomic write**: The archive is written to a temporary `.partial` file and renamed upon completion. A failed backup never leaves a corrupted target file.

If your data lives in a custom directory, pass `--data-dir <PATH>`:

```sh
bot --data-dir /path/to/data backup ./praxi-backup.tar
```

## Restoring from a backup

Run `bot restore <FILE>` to unpack a backup archive into the active data directory:

```sh
bot restore ./praxi-backup.tar
```

Output:

```text
(secret backend: OS keychain)
Restored 5 files from ./praxi-backup.tar (created with Praxi 0.1.0).
API keys are not part of a backup: enter them again (`bot secrets set`, or the app's settings).
```

The first line names where `bot` keeps secrets; it can differ on your system.

### Restore requirements and flags

- **Stop the daemon first**: Restoring replaces active databases. If a daemon answers on the service port, the command refuses to run:
  ```text
  cannot restore while a daemon is running on port 47821; stop `bot serve` (or close the desktop app) and retry
  ```
- **Existing bots and chats (`--replace`)**: If the target installation already contains bots or chats, `bot restore` refuses to unpack by default to prevent accidental data loss:
  ```text
  this installation already has bots or chats; re-run with `--replace` to swap them for the backup (the current state is kept in a `backup-*` folder)
  ```
  Pass `--replace` to swap the existing bots and chats for the backup. The state being replaced is preserved in a `backup-*` folder inside the data directory, and the command tells you where:
  ```sh
  bot restore ./praxi-backup.tar --replace
  ```
  ```text
  The previous state was kept in <data dir>/backup-<number>.
  ```
- **Permission policy changes (`--allow-policy-relaxation`)**: If the backup's permission policy is more permissive than the one currently active in the installation, the restore refuses:
  ```text
  the backup's permission policy is more permissive than the current one; re-run with `--allow-policy-relaxation` to accept it
  ```
  Pass `--allow-policy-relaxation` to acknowledge and accept the more permissive policy:
  ```sh
  bot restore ./praxi-backup.tar --allow-policy-relaxation
  ```

## Restore checklist

When restoring an installation or moving to a new machine, follow these steps in order:

1. **Stop the app / daemon**: Exit the desktop application or stop `bot serve`. Ensure no process is listening on the API port.
2. **Run restore**: Execute `bot restore <FILE>` (add `--replace` if restoring over existing bots or chats, or `--allow-policy-relaxation` if accepting a more permissive policy).
3. **Re-enter API keys**: Configure your provider keys again using `bot secrets set <provider>`, `bot setup`, or through the desktop app settings.
4. **Start the app**: Launch the desktop application or start `bot serve`.
5. **Check bots and schedules**: Verify that your bots, previous chat history, skills, workflows, and scheduled tasks are present and firing as expected.

## See also

- [CLI reference](cli-reference.md#bot-backup-file-and-bot-restore-file) for the complete list of flags and options.
- [Host operations](host-operations.md#backup-and-restore) for server-side archive procedures and systemd unit management on Linux hosts.
- [Security model](security.md) for permission policies and credential handling.
