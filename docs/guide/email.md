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

New to this? **Settings → Connections → Mail and web → Connect my email and calendar**
walks you through it (provider detection, app password link, live test, plain-words
fixes): see [Connect my email and calendar](connect-email-calendar.md).

With the daemon running you can add, change, test and remove accounts from
**Settings, Mail** in the desktop app, or from the command line:

```sh
bot email add work alice@gmail.com --preset gmail --bot Atlas --password-env MAIL_PW
bot email list
bot email test work        # IMAP login + SMTP auth, nothing is sent
bot email rm work
```

A new mailbox has no default owner: give `--bot <name-or-id>` (only that bot can
use it) or `--all-bots` (every bot that has the email tools can then read it
without asking). The app asks the same: its form starts on "Only one bot" with
no bot picked, and choosing "Any bot" shows a warning.

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

The bot panel (right side, "Library" tab) has a **Mail** block for the selected
bot: the mailboxes it may use, a "Check connection" button with a next step when
a check fails, and a switch "Let <bot> use mail". The switch adds or removes
`email_*` in the bot's `[tools] allow` / `exclude` lists (the opt-in described in
[Bots and profiles](bots-and-profiles.md)); it touches nothing else, and sending
still follows the bot's permission mode.

Each mailbox in that block can also **show recent mail** (the newest ten
headers: subject, sender, date; read-only, nothing is marked read and no body is
fetched) and **send a test mail**. The test mail is one fixed message from the
mailbox to its own address; it cannot be addressed to anyone else. Both use
`GET /v1/email/accounts/{name}/messages` and
`POST /v1/email/accounts/{name}/send-test`, which need the owner key (the desktop
app has it; a bare API token does not). On a paired host only a desktop may read the
headers, and the test mail is sent from the host itself.

### One mailbox per bot

An account can belong to one bot:

```toml
[[accounts]]
name = "atlas"
bot = "6f1c2a4e-...-uuid-of-the-bot"      # the bot's id (`bot bots list`)
allowed_senders = ["alice@example.com", "@corp.example"]   # who may reach a bot through the e-mail channel
```

- A bound account is usable only by that bot, and that bot's email tools use it
  by default when no `account` is given. Any other bot asking for it by name gets
  the same "unknown account" error as for a name that does not exist.
- Accounts without `bot` keep working as before for every bot that has the
  email tools enabled. Bots do not get the email tools by default.
- When you bind an account to a bot (`bot`, `shared_with` or `default_for`) in the
  app, the bot gets the email tools by itself: `email_*` is added to its `[tools] allow`
  list (or taken out of `exclude`) and the change is recorded in the profile as
  `[tools.mail_auto]` and in the audit log. Settings, Mail shows "<bot> can now read and
  search this inbox; sending always asks you" with a "Turn off" link. Removing the last
  binding, or the account, takes back exactly what was added, never a tool you added
  yourself; if you switched the tools off by hand an account edit does not switch them on
  again. Only the app (with the owner key) can bind an account, so a bot cannot give
  itself mail tools. Sending is unchanged: the identity's send mode and the approval card.
- Deleting a bot **removes** the accounts bound to it, together with the
  passwords the daemon stored for them. They are never turned into shared
  accounts, which would give every bot access to that mailbox. If a stored
  password cannot be deleted at that moment, its name is kept in
  `email-secret-cleanup.json` and deleted on the next start.
- Bundle exports with "all secrets" leave out `email/<account>/password`
  secrets; a mailbox password is exported only when you name it.
- `allowed_senders` decides whose mail the e-mail channel may pass to a bot. The
  list is empty by default and an empty list passes **nothing** on (there is no
  "any sender"): name at least your own address. Mail from anyone else is
  ignored, counted and logged without its text. A mail that cannot be handed to
  the bot is retried three times and then skipped (counted), so it cannot block
  later mails. Mails above `max_per_poll` stay unread and come at the next check.
  A channel that was switched on before this rule existed and has no list keeps
  its settings but passes nothing on; the channel card shows a notice until you
  add a sender. A schedule trigger of kind `email` follows the same rule (one
  shared matcher): an entry with `@` is an exact address; `corp.com` or
  `@corp.com` is that domain or a subdomain of it (`mail.corp.com`, never
  `corp.com.evil.test` or `evilcorp.com`). Only the bare address counts, never
  a piece of the display name, and there is no "contains" mode. The From
  header is not authenticated (no DMARC check), so a spoofed owner address
  passes where the sender's domain does not enforce DMARC.
- The "Test connection" action logs in over TLS exactly as configured and
  reports one status per server (`ok`, `unreachable`, `tls`, `auth_failed`,
  `protocol`, `timeout`); it never sends or reads mail and never shows what the
  server said.

## Available tools

Once enabled, nine tools are registered under the `Messaging` permission category (eight from the connector, plus `email_wait_for` from the built-in tools):

| Tool | Effect | Description |
| --- | --- | --- |
| `email_identities` | Read | List the mail identities (addresses) the bot may use, its default and each send mode; no secrets |
| `email_list_folders` | Read | List folders and attributes via IMAP LIST |
| `email_search` | Read | Search messages with sender/subject/body-text/date/unread filters; returns UID summaries. `include_junk` also looks in spam: the server's junk folder of an IMAP account (merged newest first), spam and unauthenticated mail of an API mailbox; every match says its `folder` and `from_junk`, and the answer says when some came from spam. Body-text search (`text`) is IMAP only: an API mailbox says it cannot, and the bot searches by sender or subject and reads the message |
| `email_read` | Read | Read message content and attachment metadata by UID using BODY.PEEK |
| `email_mark_read` | Write | Set or clear the Seen flag on a message |
| `email_download_attachment` | Write | Save one attachment of a message (by the `id` `email_read` lists) in the bot's workspace (`downloads/email/<account>/`; the daemon's `email-attachments/<account>/` folder when workspaces are off), up to 10 MiB and within the bot's workspace limit. The file name is the sender's, reduced to a plain name (last path component only, odd characters become `_`, never overwrites: a taken name gets `-1`, `-2`); a program or script, and any file named like one (`.exe`, `.js`, `.sh`, ...), gets `.bin` added and no execute permission. Returns the saved path, size, the type the bytes really have (not the sender's word; a mismatch or a program carries a `warning`), and the text of a small file only when its bytes really are text (an HTML page is reduced to what a person sees). The file is a stranger's: it counts as untrusted, and a run that saved one no longer gets the Full-level shell waiver for interpreters and build tools. Never opens or runs it |
| `email_draft` | Write | Save a message as a draft in the drafts folder (never contacts SMTP) |
| `email_send` | Destructive | Send an e-mail immediately over SMTP (**requires human approval**). With `reply_to_uid` (or `reply_to_message_id` for API mailboxes) it replies in the thread: recipient, `Re:` subject and the `In-Reply-To`/`References` headers come from the original, and the approval card shows exactly that, and nothing is sent if the original is not the one you were shown. When the mail has a `Reply-To` that differs from the sender, `email_read` shows it and the answer goes there. **One reply needs no card:** a reply, from a bot's own mail identity, to a message whose reply address is one you listed as yours (an exact address in the account's `allowed_senders`, not a domain), going to that address and nobody else, in the thread it came from, and with no warning except the repeat-recipient one (a conversation with you is not cold mail): the opt-out list, the daily limit and a dropped opt-out line still stop it. The tool checks all of this against the mailbox and your settings at the moment of the call and the model names no recipient, so it cannot vouch for itself; the reply is still threaded, logged in the send log, and the audit log names the grant `mail.owner_thread_reply`. Every other send asks, as below |
| `email_wait_for` | Read | Wait for ONE expected mail (a verification mail) in the bot's **own** inbox and its junk folder; returns sender, subject, time, codes and the links it may follow. See below |

## Waiting for a verification mail

`email_wait_for` is how a bot finishes a sign-up: right after it submitted the form it asks for the mail from the site, and gets the code and the link it may open.

- **It does not hold the run.** The wait runs in the background for up to 10 minutes (`timeout_minutes`), but one call blocks for 30 seconds at most (`block_seconds`, 90 at most). If nothing has arrived the answer is `pending` with a `wait_id`: the bot goes on with other work (the next site) and calls again with that id. Mail that already arrived is found at once. Stop ends the call and the background wait. A daemon restart forgets the wait ids; asking again finds mail that already arrived.
- **A wait belongs to its run.** Its `wait_id` (a random id) works only in the run that started it, for the same bot and mail account; another run of the same bot (a scheduled one, say) can neither join, collect nor stop it. A wait ends with its run, and a finished wait is never reused: asking again polls afresh.
- **Junk folders are polled too.** For IMAP the server's junk folder (flagged `\Junk`, or named Junk, Spam, Bulk, Önemsiz, Gereksiz) is checked next to the inbox; for AgentMail spam and unauthenticated mail are included. Mail in junk is easy to forge, so a match from there **never completes a wait on its own**: it counts only when its sender address is on the site the browser was on when the wait started AND the mail server's `Authentication-Results` header says SPF, DKIM or DMARC passed for that domain (the topmost header, the one your own server wrote). A provider that reports nothing (AgentMail) proves nothing. Anything else is only reported when the wait ends, as a `junk_candidate` (sender, subject, time, folder and why it was not used), without its codes or links, and the bot tells you it is in spam. A mail that does qualify says which `folder` it was in and `from_junk: true`, with a `junk_note`. The junk folders are one definition everywhere (`\Junk`-flagged or named like one, at most two), and a search that includes junk reads each folder up to the limit (at most 100), within 30 seconds in all.
- **Tracking links are looked through, one hop.** Mail from SendGrid, Mailgun, Mailchimp (and Mandrill), Postmark, AWS SES (`awstrack.me`) and similar services wraps the real link in a click-tracking redirect. For these known hosts (https on the standard port only; at most three links per mail) the daemon makes one plain `GET` request (through the same address guard and the same deny lists as web fetches: no private, loopback or metadata addresses, DNS pinned to the checked addresses, no proxy, no cookies or credentials, no redirect followed, 5 seconds at most) and reads where it redirects, without opening that target. The resolver (`WebResolver` in `crates/connector-email/src/wait.rs`) refuses any host that is not a known tracking provider, any port but 443 and plain http by itself, whoever calls it. Stop ends it at once. The approval card says so. The target is then judged like any other link: only an https link on the site the browser is on (or a domain you listed) is handed to the bot.
- Everything in the answer comes from the mail and is untrusted data.

## Check that the bot's tools work

Ask the bot to run **`tool_self_check`** ("which of your tools work?"). It makes one fixed, read-only sample call per tool that has a safe one, **only for tools the bot may call without asking you** (a tool the bot cannot see, that its permissions deny or that would need an approval is `skipped`, never called, and no approval card is opened), and reports `ok`, `failed` (with a plain reason and fix hint) or `skipped` (with the reason), plus yes/no facts about the computer: does the data folder exist and is it not read-only, is there a home folder, is a shell on the PATH. It never calls a tool that changes something, makes no web search, writes nothing to disk and returns no paths. Every sentence in the answer is fixed text of the app: what a mail server, a file or a page said about a failure is never copied into it, and mail or file content is never returned. Browser and desktop checks, and the tests that write, are in `bot doctor`.

## Several mail identities

You can add several mailboxes (your own IMAP/SMTP mailbox, an AgentMail address for the bot, ...) in **Settings > Mail**. For each one you choose:

- **Who can use it**: any bot, or only the bots you pick.
- **Default sender for**: the bots that send from it when they name no address. A bot has one default.
- **Sending**: *Ask me each time* (default) or *Drafts only*. A drafts-only address saves what a bot wants to send as a draft and never sends it. This holds on every path: the mail channel does not queue replies from it, an approval cannot send from it, and **Send test mail** is refused for it (the test would send a real message).

The bot lists its identities with `email_identities`, picks one from the task ("send from my work address") and says which one it used. The approval card always shows **From**, To, subject and text; choose another address on the card and the bot is asked to send the same message from it, so you read the new card in full before anything leaves. Passwords and API keys stay in the secret store and are never shown to the model.

The shared `agentmail.to` domain is not suitable for cold mail; use an address on your own domain for outreach.

## Sign-ups: a bot with its own address

A bot that has a mail identity of its own (an address bound to it or shared with it, such as an AgentMail address) is told so at the start of every run: its address, that verification mails for sign-ups arrive in that inbox, and which mail tools it may use (the block is fixed text built by the app from your settings, never from a page or a mail). A bot with no identity of its own gets no such block, and never sees another bot's address.

For "sign up to these sites" the bot uses:

1. `account_register`: the app makes the password (the bot never sees it), keeps it in the vault for that site and that bot, signs up with the bot's address (the login is the address unless the site wants a user name) and records **which mail identity** the account was registered with.
2. `email_wait_for`: after the form is submitted the bot waits (up to 10 minutes; Stop cancels at once) for the verification mail. The filters the bot passes (sender domain, subject, keyword) are applied inside the app to what the inbox returns: nothing the bot wrote is sent to the mail server. The answer is short and labelled untrusted: sender, subject, time, one-time codes, and links. The wait is tied to the page the browser was on when it **started**, so the bot can go on to the next site while it waits; it is kept when the run ends (a long job is cut into sessions) and the bot's next session picks it up again. Only mail from shortly before the wait started counts, and a mail already handed to one site's wait is not handed to another's, so many sign-ups with one inbox do not mix up their mails.
3. A link is returned as **followable** only when it is `https`, has no user name or IP address in it, and is on the same registrable domain as the page the browser was really on when the wait started (asked from the browser, not from the page or the model) or on a domain you listed in Settings > Mail > Safe sending ("Extra sites a verification e-mail may link to"). Every other link is listed by host only as "external link, not followed"; plain `http` is refused. A link the bot was given in an earlier session (kept for a day) is remembered together with that site: the next session opens it without asking only while the browser is on the same site and the link still passes this check; on any other page it asks.
4. The bot opens a followable link with `browser_open`, confirms it is signed in, and reports. CAPTCHA, a phone number, or a code sent by SMS or e-mail that the bot cannot read go to you through **Take control**. In a **mission**, that step is parked for you on the page: the tab stays open, you get a notice, open **Live view**, finish the check, and the mission retries the site later from the same page. An authenticator-app code the bot can handle itself (see below). The playbook is the bundled skill `signup-to-site`.

### Two-factor codes for the bot's own accounts

When a site makes a bot-made account set up an authenticator app, it shows a setup key: an `otpauth://` address (the text behind the QR code) or a base32 key. The bot can keep that key with the account and type the current 6-digit code itself:

1. `account_totp_save` with the account's secret name (from `account_list`) and the element of the page that shows the key (a `ref` from the snapshot, or a CSS selector). **The app reads the key off the page itself**: the bot never types or copies it, and a key shown on a page is masked in what the bot reads. The card names the account, the site and the element, never the key. On the **Full** level, in a run you started, nothing asks for the bot's own accounts (below); on every other level you approve each save.
2. When the site asks for a code, `browser_fill` with `secret` set to `totp:<account name>`. The app works out the current code (RFC 6238; SHA-1, SHA-256 and SHA-512, 6 to 8 digits) and types it into the field. On the **Full** level, in a run you started, it types without a card, like everything else on Full except spending money. On every other level **every typing asks you**, like a password fill, and a standing "always allow" for the password does not cover the code.

What is guaranteed in code, not left to the bot:

- **The key and the codes never reach the model.** The key is kept in the vault, apart from the password, and is never listed, revealed or returned. The code goes straight from the app into the page field. Both are masked in every result, log and page text, in every way a site writes the key (with spaces or dashes, in either case, with or without `=` padding, or as the whole `otpauth://` address).
- **Bound to that bot and that site.** Only the bot that registered the account can save or use its key, and a code is typed only into a field on the account's own site: the live frame and the page are checked before the card and again inside the step that makes the code, so no other way of asking gets one. A desktop program never receives a code, and a code is never revealed or covered by a standing grant.
- **Only for the bot's own accounts.** Your own secrets and logins never get a code here. Asking for one is refused.
- Saving again replaces the key. Discarding or deleting the account deletes its key too.

A `hotp` (counter based) address is refused. If the site only offers SMS, e-mail or a security key, the bot stops and asks you (in a mission, the item is parked for you on the live page instead of being given up).

Permissions: reading the bot's **own** inbox (`email_search`, `email_read`, `email_list_folders`, `email_wait_for`) and opening a link `email_wait_for` returned as followable run **without a card** only for a run **you started** by hand, on a bot at the **Full** level. A run started by mail, a webhook, a schedule, a channel or an imported chat keeps asking, as before. An inbox every bot may use (an unbound account) and your personal mailbox are never "the bot's own". Sending is unchanged: `email_send` asks (except a reply to your own address in its own thread, see the tool table), and a draft-only address only saves drafts.

**Several accounts, a record of the work, and keys the page shows.** `account_register` keeps one account per site and login. A further account on the same site needs a different login and a short `label`, and `account_list {"domain": "example.com"}` shows what the bot already has there (a bot checks it before it registers, so a long list of sites never gets the same site twice). `account_discard` removes an entry. `site_ledger` is the bot's own list of sites with a state (candidate, account created, waiting for verification, done, blocked, skipped), the page that shows the result and a short note. It is kept between runs, so the next session carries on where the last one stopped; it is plain text the bot wrote, never an instruction. When a page shows an API key or token after the sign-up, `browser_capture_secret` reads it from the page into the vault, limited to that site and to that bot: the bot gets a secret name and nothing else, and types it later with `browser_fill`. The page hides the value from snapshots and pictures until the bot leaves it.

On the **Full** level a run you started, or a mission of yours, signs up on its own: `account_register`, typing the password of an account **the bot made itself** (`browser_fill` with `secret`) and `browser_capture_secret` run without a card, also after the run read pages or your mail. The password is still typed only into the exact site it was registered for (the frame and the page are both checked at the moment of typing), the secrets you stored yourself still ask on every use, and money always asks. Other levels, and runs started by mail, a webhook, a schedule or a channel, ask for each of them as before.

## Approvals and security

- **Sending requires approval**: `email_send` is an external effect marked `Effect::Destructive`. It sends an e-mail to external recipients and cannot be undone. Under standard permission presets (`approve-destructive`, `read-and-draft`), the engine always prompts for explicit approval before dispatching mail.
- **Mandatory TLS**: IMAP requires implicit TLS (port 993). SMTP requires implicit TLS (port 465) or mandatory STARTTLS (port 587). Hostname and certificate validation against Mozilla roots are always enforced; plaintext connections and insecure certificates are rejected.
- **Safe credentials**: Passwords are never echoed or written to logs; credentials resolve lazily from the secret store when establishing connections.
- **Drafts**: `email_draft` saves directly to your IMAP drafts folder and never contacts SMTP.

## Safe sending (send log, opt-out list, limits)

These guards live in code, not in the model. Settings > Mail > **Safe sending** shows and edits them; the routes (`/v1/email/guard/*`) need the owner key.

- **Send log**: every e-mail that was really sent is recorded (time, bot, run, account, recipients, subject start, message id, whether the opt-out line was added, whether you chose "Send anyway"). You can open it in the app and export it as CSV. It is stored in `<data dir>/email-guard/send-log.jsonl`.
- **Opt-out list**: addresses or whole domains you will not write to (`email-guard/opt-out.json`).
- **Opt-out line**: the app appends it to the body of every `email_send` (default text in Turkish and English, editable, `{sender}` = the sending address). The bot does not write it and cannot remove it. A bot may ask to leave it out (`opt_out_line: false`); that only makes the approval card show a warning.
- **Limits**: a warning above 20 e-mails per 24 hours per account, and when someone got mail from the account in the last 14 days. Both numbers are settings (0 turns a warning off).
- **Warnings are not blocks.** When a send hits the opt-out list, a limit or the missing opt-out line, the approval card lists the warnings (and once, the note that ignoring an opt-out on commercial mail can be unlawful, in Turkey under Law 6563, ETK) and its button reads **Send anyway**. Only you can click it; it is logged. A model cannot override a warning: the tool accepts a send past a warning only when it matches, exactly, a card that was shown to you, once.
- `email_send` asks, whatever the permission preset or grants, with two exceptions. A reply to an address you listed as your own, in the thread it came from, needs no card (see the tool table); below Full access a run that has read untrusted content keeps asking for that reply too. On **Full** access nothing asks except spending money, so a mail without a warning goes out unasked, also after the run read untrusted content; a mail that hits a warning above (opt-out list, limit, missing opt-out line, and for a stranger the repeat recipient) is still stopped, not sent.

## Give a bot its own address (AgentMail)

Besides your own mailbox, a bot can have an address such as `name@agentmail.to`,
created at [AgentMail](https://agentmail.to) with **your own** AgentMail account
and API key. Praxi-Bot holds no key of its own and runs no relay.

- In **Settings, Mail** choose "Give this bot an address (AgentMail)",
  paste your API key, pick the bot (or any bot, explicitly) and, optionally, a name
  for the address. From the command line:
  `bot email add --provider agentmail --name helper --bot Atlas --api-key-env AGENTMAIL_KEY`.
- A key that belongs to one inbox (`am_us_inbox_...`) cannot create inboxes. Choose
  "Connect an existing inbox" in the same form, paste that key and the address of the
  inbox: the key is checked with a read of that inbox (`GET /v0/inboxes/{id}`), nothing
  is created, and the account works like any other AgentMail account. In the API this is
  `POST /v1/email/accounts` with `"inbox": "praxi@agentmail.to"`.
- The key is write-only: it goes to the secret store as `email/<account>/api_key`
  (excluded from state bundles unless you name it) and is only ever sent, in the
  `Authorization` header, to `api.agentmail.to` over HTTPS. Redirects are never followed.
- `email.toml` stores `kind = "agentmail"` and the `inbox_id`; older files without a
  `kind` are IMAP/SMTP accounts, as before.
- The same `email_*` tools work. For an AgentMail account messages are identified by
  `message_id` (shown by `email_search`) instead of an IMAP `uid`; folders and drafts are
  not supported and say so.
- Removing an account (or deleting a bot) only forgets it locally. The inbox at AgentMail is
  deleted only when you ask: `bot email rm <name> --delete-inbox`, or the checkbox in the app.
- At the time of writing the free plan allows 3 inboxes, 3,000 e-mails a month and 100 a day;
  see their [pricing](https://agentmail.to/pricing). Cold outreach is prohibited on the shared domain.
