# Email integration

Praxi includes IMAP and SMTP tools that allow your bots to read, search, draft, and send e-mails.

## How to enable email

Email support requires two things:
1. Store your mailbox password (or app-specific password) in the Praxi secret store.
2. Configure your email account in `<data_dir>/email.toml` (or under the `[email]` section of `providers.toml`).

### 1. Store your password

Mail passwords are never stored in plain text configuration files. They are referenced by name via `SecretRef` and retrieved from the secret store only when connecting:

```sh
bot secrets set email/work/password
```

Type or paste your password when prompted.

### 2. Configure accounts

Create or edit `email.toml` in your Praxi data directory (the default per-user data directory, or the path passed via `--data-dir` / `BOT_DATA_DIR`):

```toml
[[accounts]]
name = "work"
address = "alice@example.com"
username = "alice@example.com"      # optional: defaults to address
password = "email/work/password"   # SecretRef handle stored in step 1

[accounts.imap]
host = "imap.example.com"
port = 993                        # default: 993 (implicit TLS)

[accounts.smtp]
host = "smtp.example.com"
port = 465                        # default: 465 (implicit TLS) or 587 (STARTTLS)
security = "implicit"             # "implicit" or "start_tls"
```

You can configure multiple accounts with distinct `name` identifiers.

Alternatively, you can place an `[email]` section directly inside `<data_dir>/providers.toml`:

```toml
[email]
[[email.accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"

[email.accounts.imap]
host = "imap.example.com"

[email.accounts.smtp]
host = "smtp.example.com"
```

### 3. Start the daemon

Start the daemon with `bot serve`:

```sh
bot serve
```

When the secret store is available, the daemon registers the email tools once and prints, for example:
```
email: 6 tool(s) registered (1 account(s))
```

A bot is only offered the email tools when at least one account is usable by it
(see below), so registering them with no account configured does nothing visible.

## Managing accounts without editing files

With the daemon running you can add, change, test and remove accounts from
**Settings, Computer, E-mail** in the desktop app, or from the command line:

```sh
bot email add work alice@gmail.com --preset gmail --password-env MAIL_PW
bot email list
bot email test work        # IMAP login + SMTP auth, nothing is sent
bot email rm work
```

Presets exist for `gmail`, `outlook`, `yahoo` and `icloud`; for anything else give
`--imap-host`, `--smtp-host` and, if needed, `--imap-port`, `--smtp-port`,
`--smtp-security start_tls`. The password is read from the variable named by
`--password-env` or from a hidden prompt, never from a flag. Most providers need
an app password.

Changes are written to `email.toml` and take effect for the next tool call; no
restart is needed. The password goes to the secret store as
`email/<account>/password` and is never returned by the API, the CLI or the app
(the account list only says whether one is stored). Changing an account's
servers or login requires entering the password again, so a stored password can
not be redirected to another server.

### One mailbox per bot

An account can belong to one bot:

```toml
[[accounts]]
name = "atlas"
bot = "6f1c2a4e-...-uuid-of-the-bot"      # the bot's id (`bot bots list`)
allowed_senders = ["alice@example.com", "@corp.example"]   # stored; used by a later release
```

- A bound account is usable only by that bot, and that bot's email tools use it
  by default when no `account` is given. Any other bot asking for it by name gets
  the same "unknown account" error as for a name that does not exist.
- Accounts without `bot` keep working as before for every bot that has the
  email tools enabled. Bots do not get the email tools by default: add `email_*`
  to the bot's allowed tools.
- Deleting a bot **removes** the accounts bound to it, together with the
  passwords the daemon stored for them. They are never turned into shared
  accounts, which would give every bot access to that mailbox.
- `allowed_senders` is validated and stored but nothing reads incoming mail yet.
- The "Test connection" action logs in over TLS exactly as configured and
  reports one status per server (`ok`, `unreachable`, `tls`, `auth_failed`,
  `protocol`, `timeout`); it never sends or reads mail and never shows what the
  server said.

## Available tools

Once enabled, six tools are registered under the `Messaging` permission category:

| Tool | Effect | Description |
| --- | --- | --- |
| `email_list_folders` | Read | List folders and attributes via IMAP LIST |
| `email_search` | Read | Search messages with sender/subject/date/unread filters; returns UID summaries |
| `email_read` | Read | Read message content and attachment metadata by UID using BODY.PEEK |
| `email_mark_read` | Write | Set or clear the Seen flag on a message |
| `email_draft` | Write | Save a message as a draft in the drafts folder (never contacts SMTP) |
| `email_send` | Destructive | Send an e-mail immediately over SMTP (**requires human approval**) |

## Approvals and security

- **Sending requires approval**: `email_send` is an external effect marked `Effect::Destructive`. It sends an e-mail to external recipients and cannot be undone. Under standard permission presets (`approve-destructive`, `read-and-draft`), the engine always prompts for explicit approval before dispatching mail.
- **Mandatory TLS**: IMAP requires implicit TLS (port 993). SMTP requires implicit TLS (port 465) or mandatory STARTTLS (port 587). Hostname and certificate validation against Mozilla roots are always enforced; plaintext connections and insecure certificates are rejected.
- **Safe credentials**: Passwords are never echoed or written to logs; credentials resolve lazily from the secret store when establishing connections.
- **Drafts**: `email_draft` saves directly to your IMAP drafts folder and never contacts SMTP.
