# Security model

An agent that can use your computer is a powerful thing and a tempting target:
a web page or document can contain text that tries to give the bot orders
("prompt injection"). Praxi's design assumes that can happen and puts hard
limits in code, outside the model's reach. This page says what those limits are
and, just as important, what they are not.

## What protects you

**1. Permissions are enforced by code, not by the model.** Every tool call goes
through a resolver that looks at the tool's category, the bot's policy and who
started the run. The model cannot talk its way past it. Details and presets are
in [Chatting and approvals](chatting-and-approvals.md).

**2. Defaults ask first.** With the default preset, anything that changes files,
clicks or types in apps, runs commands or sends messages needs your approval.

**3. Absolute floors that no setting removes.** Money actions always ask. A
parsed never-list of catastrophic shell commands (disk formatting, deleting a
drive root, your home folder or system directories, and so on) always asks, even
in the `full` preset and even if you once chose "always allow". Subagents can
never be more permissive than their parent bot.

**4. Scripts and workflows do not bypass anything.** `script_run` and
`workflow_run` call tools through the same gate, checking every inner call.

**5. A local-only daemon.** The API binds `127.0.0.1` only. Each request is
checked for a loopback `Host` header, a browser `Origin` check blocks web pages
from calling it (only the desktop app and origins you list in
`BOT_ALLOWED_ORIGINS` pass), and calls need a bearer token: 256 random bits in
`<data dir>/api-token`, readable only by your user (`0600` on Unix, restricted
with `icacls` on Windows; the daemon warns if that fails). Two routes skip the
token and require a valid HMAC signature instead: `POST /v1/webhooks/{id}`
(a scheduled job) and `POST /v1/incoming-webhooks/{id}` (a bot webhook). The
incoming webhook also requires a fresh timestamp, accepts that signature once,
and treats the body as untrusted data. See [Incoming webhooks](incoming-webhooks.md).

**6. Secrets stay out of files and logs.** API keys and the Telegram token are in
the OS keychain or an encrypted file, never in `providers.toml` or on a command
line (`--key-env` and hidden prompts only). Output sent to Telegram is passed
through a redactor that masks known secrets.

**7. An audit trail.** Approvals, denials and grant changes are written to an
append-only audit log in the database; the log cannot be edited or deleted
through the API, and database triggers enforce it.

**8. Take-over and stop.** You can take control or pause a bot's desktop access
from the desktop app, and the global hotkey (default `Ctrl+Alt+Esc`) stops it.
While you are driving, the bot's desktop actions are refused.

**9. Undo for files.** File changes are checkpointed before they happen.

**10. Isolation of the browser.** Browser tools use their own isolated
profile by default, not your everyday browser with its logged-in sessions.

**11. No telemetry.** The project decision (ADR-0020) is that Praxi does not
phone home. Data goes to the model provider you configured and to nowhere else,
except what a bot sends on your behalf when you approve it.

**12. Egress after a read asks.** Once a run has put untrusted or private content
in front of the model (a file, a mailbox, a page, a screenshot, an MCP result,
a remote tool description, recalled memory, a workflow report), any tool that
can send data off the machine asks, even when that tool is a read and even when
you have a standing grant for it. Typing, dragging, launching a program, and
changing browser tabs ask. Bare clicks and scrolls remain prompt-free; native
file pickers are a known upload risk (see below). Mailbox reads, searches, and mark-read ask, because those calls
send the model's arguments to another machine. Saving or publishing a skill
after such a read asks too, and the skill is stored as untrusted data: later
runs see it only when they call `skill_list` or `skill_read`; it stays out of
automatic prompts and router suggestions. A search that can only reach the search
provider you configured stays prompt-free after untrusted content, until the
run has also read something private. That prompt-free path still allows emoji
variation selectors, ZWJ, ZWNJ, LRM/RLM/ALM, and CR. Later turns of the same
chat stay gated while a message that still carries that content is in the
prompt. The only way to skip this ask is an explicit per-bot setting, "allow
sending data after this bot has read something", which you turn on in the bot
form. That waiver skips the ask. It does not allow characters that cannot be
shown: hidden tags, bidi overrides and isolates, invisible operators,
private-use characters, and U+FFFC are refused even then. The card shows the
outgoing text in full, with non-printing characters revealed as `<U+XXXX>`,
line and character counts, and counts of spaces and tabs in every collapsed
blank or trailing whitespace run. Above 8 KB, the desktop requires opening
the full payload and reaching its end before approval. CLI and Telegram print
the complete text before offering approval. An empty tool allow list is
not a bypass. Deny rules and the floors in item 3 still win. A run that has
not read anything is unchanged, and existing grants still apply to it.
Approving one send does not create a standing grant. The card lists every
recipient and every field that will be sent, including a default browser
profile, the search backend, and a default mail account. Email attachments
are covered by `email_read`; there is no separate attachment tool. When a
call does carry an attachment list, the card shows each name and size.

**12a. A browser you connected as a browser.** An MCP server you connected
as a browser (the built-in Playwright preset, or any MCP server whose
configuration has `browser = true`, set in `mcp.toml` or through the
owner-only API; a server, a page or a model can never set it) acts inside
that browser. Its tools are judged by the name the server registered, from a
fixed list, never from the model's arguments:

| Tool of such a server | Clean run | After a web page only | After private data (files, mail, memory, your screen) too |
|---|---|---|---|
| In-page action: `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_evaluate` (a script inside the page), `browser_wait_for`, `browser_tabs`, `browser_take_screenshot`, ... | level decides | **no card** on Full; on "ask on destructive" too (they are not destructive) | asks |
| Code outside the page (`browser_run_code`, `browser_run_code_unsafe`, anything named like code, script, exec, install) | **always asks** | always asks | always asks |
| File upload or download (`browser_file_upload`, `browser_pdf_save`, ...) | **always asks** | always asks | always asks |
| Any other tool of the server | as any MCP tool | asks | asks |

**Addresses.** A call that opens a URL (`browser_navigate`, `browser_tabs`,
`browser_tab_new`) stays prompt-free only when its URL argument is an `http`
or `https` address with a public DNS name or public IP, no user name or
password. A `file:` path, `about:`, `chrome:`, `data:`, `javascript:`,
`view-source:`, `blob:`, `ftp:`, `localhost`, `*.local`, `*.internal`, `*.lan`,
`*.home.arpa`, a single-word host, a loopback, private (10/8, 172.16/12,
192.168/16), link-local, carrier-grade (100.64/10), unique-local or other
reserved IP (also written as one number, in hex or octal, or inside an
IPv6 form), an address that does not parse, a missing, non-text or very long
URL, always asks, in every run, with the plain sentence "open <address> in
your browser". "Allow for this task" is not offered on that card, and an
earlier grant never covers such a call: the address is checked again at every
call. Once you approve one, the run counts as having read private data, so
every later in-page action that can carry data, a script included, asks. Links
that the page itself follows, or a redirect, are not judged by the bot; the
address bar of the browser decides there.

Before this rule every call of such a server counted as "sending data off
the machine" and its result as private, so a five-minute task in a page could
need a hundred cards. A page you opened in that browser is web content, not
your private data: an in-page result counts as untrusted, not private. The
consequence to know: if you sign the automation browser into private
accounts, a page that injected instructions can make the bot open and read
those accounts and navigate elsewhere without a card, as long as the task has
not read anything private through other tools. A name that resolves to a
private address (DNS rebinding) cannot be seen without resolving it, so a
navigation to such a public-looking name passes without a card; that is an
accepted residual like the signed-in accounts above. The Playwright preset uses its
own isolated profile; the preset is recognised only in its exact catalog form
(`npx -y @playwright/mcp@<version>`), so `--extension`, `--cdp-endpoint` or a
user data directory need the explicit `browser` flag, which is your decision.
Shell, scripts, desktop typing, launching programs, sending mail and money
keep their floors; MCP servers that are not browsers, external agents and the
MCP server mode keep their stricter rules.

**12aa. A server's own tool labels.** MCP tools describe themselves with
labels (read-only, destructive, idempotent, open-world). Praxi never believes
them on its own: a tool without labels, or any tool of a server you did not
mark as trusted, asks. For a server you marked trusted only read-only tools run
without asking, an open-world tool counts as able to send data out, and a label
that changes after you approved the tool sends it back to approval. See
[MCP servers](mcp-servers.md) and the
[policy table](../design/mcp-annotations.md).

**12b. Approval scopes.** When a card for an MCP tool cannot be avoided it can
offer: **Allow once**; **Allow for this task**, the same server and tool for
the rest of this run only (never persisted, never another tool, not given to
sub-agents, and it stops covering as soon as the run reads something it had
not read when you gave it, for example private data after a web page; the
next call asks again); **Always allow for this bot (this tool)**, a standing
grant that is offered only on a card the policy alone asked for, never next
to private data and never for an always-ask tool, a payment or a never-list
command (a standing grant never waives item 12); and **Deny**. Every approval
is recorded in the audit log with its scope (`RunTool` for the task scope).
"Allow for this task" is not offered for a program start after untrusted
content, a floor, an always-ask tool or an external caller, and the daemon
refuses it on a card that did not offer it. It never covers a file, localhost or LAN address (see item 12a, Addresses);
the address is judged again at every call.

## What it does not protect you from

Be realistic about the limits:

- **Your model provider sees your prompts and tool results.** A cloud provider
  receives whatever the bot reads and works with, including file contents and page
  text. Use a local model (Ollama, LM Studio) for sensitive material. See the
  [Gemini free tier](providers-and-models.md#gemini-free-tier) note.
- **An approval is only as good as your reading of it.** A request can look
  harmless and still matter. Read the summary; deny when unsure.
- **"Always allow" and the `full` preset are standing permissions.** They do not
  waive the egress gate in item 12: after a run has read private or untrusted
  content, sending data off the machine still asks unless that bot's explicit
  setting is on. Prefer per-category rules, narrow `[tools]` lists and separate
  bots for risky jobs.
- **The never-list is a safety net, not a sandbox.** It recognises known
  catastrophic patterns; it cannot judge every harmful command. Shell access
  (`shell_run`) is as dangerous as giving someone a terminal.
- **One approved shell command can reach the daemon.** `shell_run` runs with your
  user rights. The bearer token file is readable by that user, and the daemon
  API is on loopback, so a shell command can read the token and call the API
  (approvals, takeover). The never-list and the file-tool protections raise the
  bar but are string checks, not a boundary. A real separation of the shell from
  the daemon credential is planned.
- **Shell and desktop actions are not undoable.** Only file tools take
  checkpoints.
- **Unattended runs.** Schedules run without you watching. They pause when an
  approval times out, but anything allowed by policy just runs.
- **The Windows desktop installer is not code-signed yet**, so SmartScreen warns.
  Only install builds you made or trust.
- **Early development.** The code is young and unaudited. Do not rely on it for
  high-stakes automation yet.

## Who counts as the owner on a host

On your own computer the owner is the person at the keyboard (the desktop app
holds a per-launch owner key). On an always-on host there is no keyboard, so the
host trusts one kind of paired device as the owner: a **Desktop that you
confirmed as administrator** (`sudo praxi host invite --role desktop` makes one;
`bot host admin --all-desktops` confirms desktops paired earlier). It is read
from the pairing store on every request, so a revoke or a withdrawn grant works
at once. It is one rule in one place: an administrator Desktop passes every
owner-only route, minus a short never-list. The never-list is the part to
audit: pairing and administrators, the host's lifecycle and state import and
hand over, the host's links to other computers, MCP facade credentials, doctor
repairs, Telegram pairing and the signed webhook ingress. Every change it makes
is written to the audit log (desktop, kind of change, method, path; never the
body), and the owner key itself is never sent over the link.

Secrets never travel back over that link. Keys and passwords are write-only,
the owner key and the API token are never an output, and an administrator
Desktop cannot reveal the plaintext of a stored login, password or card
(`POST /v1/fill-secrets/reveal` is on the never-list and the handler also
insists on the person at the host's own app), so a stolen administrator Desktop
cannot drain the vault. One route shows a credential once on purpose: creating
an incoming webhook for a bot (`POST /v1/bots/{id}/incoming-webhooks`, and its
`rotate-secret` sibling) returns its HMAC secret in that one answer, because whoever sets the webhook up must
paste it into the sending service; it is the owner's job and is audited.
Reads of sensitive collections by an administrator Desktop (audit export, chat
export, the list of stored secret names, the mail send log, the usage export)
are written to the audit log like changes, and a policy change also records the
coarse level before and after (never the body).

Phones, nodes, other hosts and Desktops that are not confirmed are never the
owner: of the owner-only routes they keep only what a device does every day
(deciding approvals and questions, the screen takeover buttons, timers), and a
header they send cannot make them the owner.

## Daemon process guard (defence in depth)

At startup the daemon restricts its own process so that a child running as the
same OS user (a shell command, MCP server or coding CLI) cannot read its memory,
where the daemon keeps its owner secret:

- **Windows:** the process gets a protected access list: the owner may only
  query, wait for and terminate it. Opening it for memory read/write, handle
  duplication or thread creation fails with "access denied".
- **Linux:** not enabled yet; `doctor` shows "skipped". `PR_SET_DUMPABLE=0`
  would also hide `/proc/<pid>` from the same user, which the desktop app uses
  to confirm it is talking to the real daemon, so the app would refuse to
  connect. It needs the desktop check moved to a pid+stdin handshake or
  `SO_PEERCRED` first; that is future work.
- **macOS and others:** nothing is done; `doctor` shows "not supported".

So for now this protection is **Windows only**.

Children start and run normally; they do not inherit the restriction. If the OS
refuses, the daemon still starts, prints a `DEGRADED` warning and `doctor`
(`process guard`) shows it.

What this does **not** do: it is not a sandbox. A same-user child can still read
the token file, call the loopback API with it, edit your configuration files,
automate the UI, or use a handle that existed before the guard was applied. It
only closes the in-memory route. Root/administrator and the debugger-privileged
accounts are not affected.

On Windows an attacker process running **elevated** (administrator token with
`SeDebugPrivilege`, or SYSTEM) bypasses any process access list, so the guard
cannot protect against it. After applying the list the daemon therefore tries to
open itself for memory read/write; if that still succeeds it reports `DEGRADED`
instead of claiming protection (this is what happens on an elevated
administrator account, such as the GitHub Windows runner). Run the daemon from a
normal, non-elevated account if you want this protection.

## Good practice

1. Keep the default `approve-destructive` preset; widen deliberately.
2. Create a separate, narrowly scoped bot for each kind of job (see the
   [profile](bots-and-profiles.md) `tools` and `permissions` tables).
3. Review grants now and then: `bot grants list <bot-id>`.
4. Give schedules a bot that can only do the scheduled job.
5. Use a local model for private data.
6. Treat `<data dir>/api-token` like a password; do not share your data folder.
7. Keep Telegram paired to your own account only.

## Reporting a vulnerability

Please do not post exploit details in a public issue. The repository does not
list a dedicated security contact yet (**Planned**); contact the maintainers
privately through the channel you use to reach them, and keep the report
confidential until it is fixed.


## Live screen privacy and human control

Live-view pixels can contain passwords, OTPs, private documents and notifications.
They cannot be reliably redacted. A desktop target shares the entire selected
screen; a browser target shares page content rather than OS dialogs or browser
chrome. Treat `screen:view` and `screen:input` as separate powerful privileges.
Human input into a desktop terminal carries full computer authority.

Live-view frames and human input payloads stay in the dedicated memory-only
transport. They never enter NDJSON/replay, chat attachments, audit, trace,
telemetry, or model-provider requests. The live-view last-frame cache is never
an input to screenshot tools. An explicit normal bot screenshot uses its
existing permission and data path independently. Product-managed persistence is
prohibited; OS swap/hibernation/dumps and third-party application recordings are
outside that guarantee.

Input requires the authenticated session owner to hold the active persistent
global takeover latch, with a valid epoch and current target/frame geometry.
Disconnect, expiry, revoke and restart invalidate input, reset injected keys best
effort and retain pause. A legacy release request cannot release an owned
takeover. A pause that has no takeover owner (your own Pause, also after a
daemon restart) is released by the local user through the normal release path;
a paused takeover latch (including one restored after a restart) is cleared with
`POST /v1/control/recover`, which needs no view session and no registered
target. `POST /v1/control/recover` is allowed for the local user and for a
paired **host admin desktop** that still holds `screen:input` permission (never
a phone, a non-admin peer, or a bot; an explicit revoke on the host wins). When
a host admin desktop clears a pause via `recover`, its own screen-input grant is
kept; only a local user's override revokes a peer's input grant. Resume
otherwise requires explicit owned release or privileged recovery, durable safe
continuation notes and successful reset.

You always outrank a paired device. `POST /v1/control/override` (and the app's
Pause) interrupts any takeover at once, resets pressed keys and keeps the bot
paused; only the local user can call it. A paired device has no `screen:view` and
no `screen:input` until you grant them for that device
(`PUT /v1/view/privileges/{device}`); revoking a pairing, or the grant, ends that
device's sessions and takeover within seconds. `GET /v1/control/state` shows who
drives which target and in which state; only the owner also gets the takeover id
and epoch. The global event stream carries a `control_state_changed`
notification (who, which target, state, reason; no ids, pixels or text) whenever
a takeover is acquired, becomes active, is interrupted, released or recovered.

While you drive (`human_driving`), the bot's observation tools are refused too
(snapshots, screenshots, OCR, tab and window lists): the model must not read what
you type, such as passwords or one-time codes. A plain pause keeps reads. A bot
action that waits for your pause or takeover to end waits at most 30 minutes
(the same bound as a scheduled approval) and then reports that you are still in
control; the pause itself stays. Pressing **Stop** aborts any run parked behind
a pause or takeover immediately instead of waiting out that bound.

The continuation note a takeover leaves in the chat is the only message the
model receives as a system message from stored history. It has its own role that
only the daemon writes and exactly one fixed text shape (intervention type, event
counts, target kind); a stored message with any other text or role is never
delivered as a system message, and a state-bundle import turns such rows (and any
unknown role) into the inert `imported_note` role, which is never sent to the
model; history forwards only an allow-list of roles (user, assistant, the genuine
control note) and ignores every other value. The daemon's own files (`bot.db`, `schedules.db`, `memory.db` with their
WAL/SHM/journal, `control-latch.json`, `policy.json`, `host.toml`, `config.toml`,
`mcp.toml`, `providers.toml`, `web.toml`, `fill-secrets.json`, the Telegram state files, the
`peer`, `bots`, `profiles` and `runs` folders and the bearer token) are protected
from the bot's file tools for reading and writing, including names that differ
only by a trailing dot or space or by a `:stream` suffix on Windows. Skills and
workflows use their owning tools; generic file tools cannot change them. A
shell command that names them always asks for approval. That rule is damping only: one approved
`shell_run` still runs as you and can reach those files and the loopback API
(see below). Audit stores only
actor/target IDs, event-type counts, sequence ranges, timing and fixed outcomes;
audit failure denies authorization and suspends input. See the
[live-view API and integration status](../api/live-view.md).


Untrusted skills stay out of automatic prompts and router suggestions. They are
available through `skill_list` and `skill_read`; runs that use those tools carry
their read exposure. Unrelated fresh runs stay clean. A clean `skill_write`
overwrite clears the marker after saving the replacement successfully.

Users can review and trust existing content locally without an egress waiver:
`bot skills review <name> --bot-id <id>` prints every file in full with
JSON-quoted whitespace and a fingerprint. After reviewing it, run
`bot skills trust <name> --bot-id <id> --reviewed <fingerprint>`.
Omit `--bot-id` for a shared skill. A changed file invalidates the fingerprint;
review it again. These commands are user actions, never model tools.


`fs_edit` is a private read, including failed and no-op edits: its match counts
and specific errors reveal file content. An ordinary edit therefore taints the
run even when the file was just written by the model or the text was pasted by
the user. Narrowing that would reopen the match-count oracle, so a later egress
asks. The cost is an edit that did not follow a private read.

A shell, a script, a desktop launch and desktop input (`desktop_type`,
`desktop_key`, `desktop_set_value`, `desktop_drag`) start a program or drive the
keyboard on your behalf. After a run has put untrusted content in front of the
model, each of them asks, once, with the full command or input on the card. A
standing grant, the read shortcut and `allow_egress_after_reads`
do not cover that ask. Script, desktop launch and desktop input always ask, on every
platform. The ONE exception is the Full level's shell rule below: a plain
`shell_run` of an owner-started run, judged from the whole command line against a
strict allow-list.

### What is still asked in the Full level

The Full level now asks only before spending money (see "Full access" in
[Chatting and approvals](chatting-and-approvals.md)). The rest of this section is the older,
narrower rule set that applies when the operator switches the Full lane off
(`[security]` `full_access_lane = false` in `host.toml`, or `BOT_FULL_ACCESS_LANE=off`) and to
the runs the lane does not serve.

The **Full** level ("Tam") is for an owner who wants the bot to do the work without a
card per step. It never switches a hard stop off. In a run the **owner** started it
removes only these asks, and only while every condition in the row holds:

* **Who is the owner.** The desktop app, the admin desktop, and your **own paired
  Telegram chat**: only a paired user, in a private 1:1 chat with the bot, whose message
  also carries the daemon's owner key counts. A Telegram group, an unpaired user, a bot,
  a Discord message, a webhook, a mail, a schedule, a subagent, a helper bot and an
  external agent never do.
* **Shell after a page read.** A `shell_run` on the bot's own host computer runs without
  a card only when ALL hold: the **whole command line** parses without any expansion and
  every simple command in it is on a fixed allow-list of read-only tools (`ls`, `dir`,
  `cat`, `type`, `head`, `tail`, `wc`, `find` without `-exec` or `-delete`, `grep`, `rg`,
  `sort`, `uniq`, `echo`, `date`, `whoami`, `hostname`, `uname`, `git status`, `diff`,
  `log`, `show`, `branch --list`, `rev-parse`, version queries) or local build and test
  tools (`cargo build`, `check`, `test`, `clippy`, `fmt`, `doc`, `run`, `npm`, `pnpm` or
  `yarn` `test` and `run`, `make`, `python`, `node` or `bash` with a script,
  `python -m pytest`); the flags of every program are checked; every path stays inside
  the bot's workspace (no `..`, no absolute path, no drive, no UNC, symlinks and
  junctions resolved); `>` and `>>` only write inside the workspace; no private word of
  the run is copied; at most 60 such commands per run. **Anything the parser does not
  understand asks.** There is no list of forbidden words: a network client, `start`,
  `Resolve-DnsName`, `schtasks`, `reg add`, `taskkill`, `curl` hidden as `cu""rl`,
  `c${x}url` or a hex escape piped into `sh`, `cmd /c`, PowerShell and its aliases, `$`,
  `%`, backticks, `^`, non-ASCII look-alikes and 8.3 names are simply not on the
  allow-list.
* **Scripts and project files the bot wrote.** An interpreter running a script, a build
  or test tool, `make` and `git` (they run code that lives in the project) are waived only
  while the run has **not written or changed a file that could hold code after it read
  untrusted content** (file tools are recorded when they are decided, redirects of waived
  commands too; a plain `.md`, `.txt`, `.csv` or image does not count). A script written
  after a page read, or edited after it, always asks; one written before any untrusted
  read runs.
* **Web reads and the managed browser.** After a page read with no private data in the
  run, everything passes; with private data AND a page read, only sites the owner named
  pass, and any other call only when no private word, no data shape and the send budget
  allow it.

| Still asks in Full, always | Why |
|---|---|
| Money (including Composio actions classed as money), the never-list (`rm -rf`, sign-in stores), mail and message sends, secrets typed with `browser_fill`, bot and schedule creation | floors and always-ask tools are decided before the Full rule. Other Composio actions (read, write, delete) follow the bot's level, and on Full they run without a card like any other non-money call: see [Connecting apps](apps.md) |
| A shell command off the allow-list, a wrapper (`cmd /c`, `powershell`, `sh`), quoting or expansion, a path outside the workspace, 400 characters or more | the page may have chosen the command; fail closed |
| A script, build or test after the run wrote code-like files after an untrusted read | the file may carry what the command line no longer shows |
| A private word of the run in a command or a new address, data-shaped text (keys, hashes, card numbers), private data that cannot be compared, over the send budget | the egress gate and the per-run budget stay |
| `browser_eval`, a named browser profile, a local or private-network address, a signed-in profile | not covered by Full |
| The user's own computer (`pc_*`) | only owner-started desktop runs, never Telegram |
| Anything in a run that is not owner-started, a subagent or a helper bot | they never inherit the owner's Full autonomy |
| A command the owner approved on a card | runs as shown; files it writes are not tracked (residual) |

**Host-status command exception (always-on host only):** On an always-on host
running on its own computer (`host` or `browser`, never the user's PC), a narrow
set of fixed read-only status commands (`uptime`, `whoami`, `hostname`, `uname`,
`date`, `df`, `free`, `nproc`, `id`) with an allow-listed short-flag cluster and
no arguments (at most 3 space-separated tokens, 64 characters max, no paths,
URLs, pipes, redirects, quotes, variables, substitutions, custom `cwd`,
non-`sh`/`bash` shell, or value-setting flags like `date -s`) is decided from
the command text alone: it neither taints a clean run as untrusted nor asks
under the executor gate in a tainted run. This is safe because these commands
cannot read user or outsider-written files, accept free-text targets, modify
state, or send data out. Any variation outside that exact shape—and all
policy-level asks in Ask mode (`read-and-draft`) and the floors—remain
unchanged.

Desktop input has one exception, and only where the computer can name the
program it is typing into: the **run-scoped control grant**. Read what it is
before you tick it: it is narrow, but it is **a heuristic, not a guarantee**.

- **What it is.** On a typing, key or set-value card the app may offer "This task:
  <program>". The card says: Praxi may type into <program> (its **full path**,
  never shortened) for this task. If the path cannot be shown whole on a surface,
  that surface does not offer the task-wide option at all and the call asks once.
  Approving lets this one run send those three kinds of input into that one
  program (executable path, process id and process start time, so a second copy or
  a recycled process id is another program) on that one computer, until the run
  ends. It lives in memory only: not saved, not shared with other runs, not
  inherited by a sub-agent, not kept for the next message, gone on restart.
- **What it satisfies.** Both the untrusted-content ask and a plain "ask first"
  policy for those calls, as "this chat" does for an ordinary tool. A deny, an
  "always ask" tool, usage limits and the absolute floors (money, the never-list)
  are decided first and still win.
- **Keys are an allowlist, one shape per call.** Enter, Tab, Esc, Backspace,
  Delete, the arrows, Home, End, PageUp, PageDown; Shift with Tab, an arrow, Home,
  End, PageUp or PageDown; Ctrl with A, C, X, V, Z, Y, S or F. Everything else
  asks: any Alt combination (Alt+Space and Alt+F4 act outside the program), F-keys,
  Win/Super/Cmd, Ctrl+Esc, Alt+Tab, Ctrl+N/O/W, and any list of several keys.
  A global hotkey that another program registered for one of the allowed keys is
  not excluded: if you run such a program, do not use the task-wide option.
- **Never covered.** `desktop_launch`, `shell_run`, `script_run` (their cards never
  offer the grant, and the loop also refuses to cover them by tool kind) and
  `desktop_drag` (always asks, one card per drag). Clicks never needed it.
- **Checked on the real receiver right before every send (Windows).** The backend
  does not trust the window it saw earlier. Before each send it reads, live, the
  foreground window **and the focused element**, which can belong to another
  process (an embedded browser or terminal inside a granted host): both must be the
  granted program, not elevated, not a password field (live state, not the
  snapshot), not a dialog, not an owned popup (custom-class dialogs included), and
  the window class lookup must have succeeded. A held modifier key refuses. The
  focusing click before typing checks the hit window and its top-level window and
  has no fallback. `set_value` is checked the same way before it writes and does
  not fall back to typing. Text goes out in short pieces with a fresh check before
  each. Any unavailable fact refuses.
- **Any stop withdraws the grant.** A refusal, a failed or partial send, an
  unconfirmed key release (the keys are released on every exit and the key state
  is verified) or a timeout removes every task-wide permission of the run: the
  next call asks again. The error says how many characters were already typed
  before the stop, so a retry does not duplicate them. The check narrows but cannot
  close the gap between the check and the keystroke.
- **The deny list is a heuristic.** The grant is not offered for programs whose
  **file name** is on a list: terminals and shells, editors and IDEs, Explorer,
  Task Manager and management consoles, Office applications (macro hosts) and
  script hosts, browsers, password managers, hotkey launchers (PowerToys, Everything
  and similar), remote-desktop and virtual-machine viewers, packaged-app frames and
  system sign-in surfaces; nor for elevated programs, nor when the window is a
  standard dialog or an owned popup. A renamed or copied binary of a listed
  program is **not** recognised, and a program that is not listed but has its own
  command line, plugin or macro feature gets the grant like any other. The list
  cannot be complete. Approve it for programs you would let a stranger type into.
- **What it still lets through.** Anything the granted program does with those keys
  and text, including its own shortcuts and commands. The card says that Praxi may
  type things it has read into the program.

Where the grant exists:

| Computer | Desktop input after untrusted content |
|---|---|
| Windows (this PC) | Asks once for "This task: <program>", then typing, the allowed keys and `set_value` are free in that program until the run ends or a send is stopped. |
| Windows, another computer (paired PC) | Asks on every call. A remote computer does not report process identity yet. |
| Linux (X11) | Asks on every call. The window's process id is set by the window's own program, so it cannot name a program reliably. |
| Linux (Wayland), macOS | There is no desktop backend on these yet, so the desktop tools are not available there at all. |

Where no grant is offered the behaviour is exactly the earlier one: each call
asks, once, with the input on the card.

**What it saves, measured.** Every dialog call asks: a standard or custom dialog
(Save, Open, Run, message boxes) is never covered, the keys Enter/Tab/Esc inside a
dialog included, and a Save filename that points at a startup folder, a profile
or a script is not specially flagged (it shows as ordinary typed text on its card).
For the ten desktop calls of a Notepad task after one file read (observe, launch,
type, key, type, key, set value, Ctrl+S, filename in the Save dialog, Enter in the
Save dialog) Windows asks **4** times (launch, the first typing call that carries
the grant, the filename, Enter in the dialog); without process identity the same
sequence asks 9 times. A 30-step recorded replay (one launch, 26 typing or key
steps in one program, three steps in a dialog) asks 5 times; many dialog steps or
steps in excluded programs cost one card each, so there is no small fixed bound.
Replay steps recorded as a generic tool step carry no target and always ask.

What counts as untrusted here is wider than web pages. A desktop observation
(`desktop_get_state`, `desktop_list_apps`) and a desktop click result count as well, so a desktop
task taints itself without any file read. Any file read, listing or
search (`fs_read`, `fs_list`, `fs_search`), any git read (`git_status`,
`git_diff`, `git_log`, `git_show`, `git_branch`), every document reader, and any
web, mail, MCP, skill, workflow or knowledge search read counts, including a
read of your own project, because a file can hold text you did not write.
Screen and page observation counts too: `desktop_get_state`,
`desktop_list_apps`, `desktop_click`, screenshots and browser snapshots. So a
desktop task taints itself with its first look at the screen, and from then on
each `desktop_launch`, `desktop_type`, `desktop_key`, `desktop_set_value` and
`desktop_drag` call is its own once-only card: approving one covers nothing
after it, and `allow_egress_after_reads` does not change that. A run-scoped
control grant is planned as a separate change and is not part of this one.
Listing skills counts when one of them was stored as untrusted, and listing
saved workflows always counts. Only `fs_edit`, `git_commit`, `process_list`,
`email_list_folders`, a listing of trusted skills and recalled memory that was
stored by a clean run leave a run private-only, and a private-only run is not
held by this rule. A knowledge search whose embedding provider is remote
counts as sending data: after a read it asks, like any other egress.

Consequences to expect:
- The taint lasts for the later turns of the same chat for as long as the
  messages that carry it stay in the context. A new chat starts clean.
- After the first file read in a chat, every shell, script, launch and (outside a
  run-scoped control grant on Windows) desktop input asks, once each. A standing
  "always allow shell" has no effect then.
- A scheduled run that reads something and then runs a command pauses for
  approval on every firing.
- A saved workflow replays as untrusted, so a workflow with a shell or launch step
  asks on every replay, and so does each desktop input step except where a
  run-scoped control grant for that program covers it (one card per program
  instead of one per step).

The card shows the command or the input, not the files the command will run. A
run that wrote `package.json`, a Makefile or a script earlier can have a card
that reads `npm test` and executes what the run wrote. The shell card does not
parse redirections. An approved shell still runs with the user's rights.
`desktop_click` is not covered: whether a click sequence can open a file on each
backend is not verified.

This does not make file writes safe. Outside the daemon data directory, a run
that read untrusted content can still create or change a file with `fs_write`,
`fs_edit`, `fs_move`, a document writer or a git commit under whatever grant
you gave, and a program that later trusts that file (an agent instruction file
such as `AGENTS.md` or `CLAUDE.md`, which a Claude or Codex CLI engine started
in that directory may load as instructions in a later clean run; an editor or
tool configuration file; a shell profile; a startup entry) will read it. Only
the engines' own configuration homes are protected, not project directories. A
local MCP server that can write files is not classified as a writer or an
executor. Review changes to such files yourself. Chats stored before the message
exposure migration start with no recorded exposure, so an old chat that already
holds web or mail content begins clean after the upgrade. A separate consent
design for file writes is tracked outside this change.

File tools cannot modify anything under the daemon data directory, even with
an exact file grant. Existing path resolution checks also apply to move
endpoints, symlink targets, and checkpoint restore destinations. Inert output
stores (`files`, `spill`, `screenshots`, `logs`, and the daemon-owned checkpoint
blobs) remain readable; all other stores default to protected. Skills and
workflows must use their owning tools. The same daemon boundary is enforced
for document/converter output paths (including CSV output), both in the
permission decision and in the writer itself.

A saved workflow replays as untrusted private input before its first step,
including a workflow this daemon saved itself and a legacy file.

Native CLI authority outside the daemon directory is protected too: the user's
`.codex`, `.claude`, and `.claude.json`, plus the configured `CODEX_HOME` and
`CLAUDE_CONFIG_DIR` locations. File tools cannot use exact grants, moves,
symlinks, or checkpoint restores to change those stores. Document output paths
use the same boundary. As with daemon data, an approved shell command still
runs with the user's rights and is not an OS sandbox.

Known remaining display channels include prompt-free search joiners and
formatting characters, homoglyphs and normalization variants, and literal text
that resembles a revealed marker. Native file-picker uploads driven entirely
by clicks also remain a limitation: arbitrary page JavaScript can open a picker
from any click, and no portable dialog identity or intercepted chooser is
currently wired into the approval gate. The click/scroll exception should not
be treated as an upload sandbox.

## Secrets the assistant may type (fill secrets)

A value is secret only if **you** stored it as one. In the Vault screen of the
desktop app (or through the local API, `/v1/fill-secrets`) you store a credential under a name such as
`login/github` together with the exact web origins (scheme, host and port; no
wildcards) it may be typed on, and, where the computer reports a verified
program identity, the desktop programs it may be typed into. The assistant
sees only the names and where each may be used (`secrets_list`) and types one
with `secret: "login/github"` instead of `text` in `browser_fill`,
`desktop_type` or `desktop_set_value`; it never holds the value.

* The value lives in your secret store under `fill/<name>`; the allow-lists live
  in `fill-secrets.json` (no value in it). No MCP, OpenAPI, provider, e-mail or
  skill configuration can reference a `fill/` name, and the `secret` parameter
  can only ever read `fill/` entries, never `composio/`, `mcp/` or a provider key.
* Typing a secret asks you **every time**, whatever the policy, the Full preset,
  read shortcuts or an "always allow" says. The card names the secret, the field
  and the origin of the frame that holds it (and says when that differs from the
  page you see) or the program. A field on an origin you did not allow is refused
  before any card exists; the origin is read from the browser for the live field,
  and again right before the value is inserted. The one exception is a standing
  grant you create for exactly "this secret on this origin"; it never covers
  another origin, another secret or a desktop program, and it does not waive the
  ask of a run that has read untrusted content (item 12): that card still appears.
  Turning off "Ask before use" for a Vault item creates one such grant for each
  origin it is allowed on. **Standing grants** in the Vault screen lists them all;
  revoking one takes effect immediately (bots ask you again on that origin), and
  revoking an item's last grant turns "Ask before use" back on.
* The value is read only after you approve, held in memory, typed, and kept out
  of events, the journal, approvals, tool steps, model history, recordings, tool
  output and error text. If a page echoes it back, the redactor masks it.
* Only the local owner can create, change or delete these; paired devices, bots,
  tools, MCP servers and skills cannot.

What this does **not** do: a password you paste into a chat message is ordinary
text in that chat (storing it as a secret and removing it from the message is a
follow-up UI step), and ordinary `text` fills are never masked: what the
assistant typed is what you see. The memory holding the value is not excluded
from crash dumps or swap.

- **Approvals stay click-only in voice call mode.** Voice call mode transcribes
  microphone audio in memory only (never written to disk or logged), sends the
  text as a normal chat message, and disables microphone events while a run waits
  for approval; spoken *"yes, approve"* never approves anything. See
  [Voice call mode](voice-call.md).
- **A paired server used as a browser computer (`server:<key>`) is sandboxed.**
  The server accepts `pc.browser` calls only from its paired administrator
  desktop (re-checking the pairing file on every request), blocks loopback,
  link-local/metadata, and private-network URLs (including redirects and
  subresources), refuses `browser_eval` and password-field input, redacts
  returned text, and blocks execution if the server audit log cannot be written.
  Local approvals and grants are scoped to `server:<key>`. See
  [Using a paired server as a bot's browser computer](remote-host.md#using-a-paired-server-as-a-bots-browser-computer).

### Letting the assistant ask for a credential

When the assistant needs an existing login, key, or backup code for a
website, it can ask you through a secure form in the desktop app. The form
shows the bot's reason and the exact HTTPS address where the credential can
be used. Enter credentials in the form. Use it only for a task and address
you recognize. Payment credentials are out of scope.

Praxi saves the credential in the local vault for that bot and address.
The bot learns whether it was saved and uses the entry by name. Both fields
of a login are masked in the form. The app hides the window from screen
capture where the platform supports it (Windows and macOS).

If you paired Telegram or Discord, your private notification contains a link
to the request's chat in the desktop app. The form is available under
**Needs you** as well. You can cancel the request or let it expire.

In a [mission](missions.md) the form does not stop the work. The item that
needs the credential is parked, the rest of the mission carries on, and the
form stays open for 24 hours. Saving it releases only that item; cancelling or
letting it expire leaves the item parked.

Requests are on by default for the local desktop with a vault. You can turn
them off by setting `BOT_SECRET_REQUESTS=off` or adding `secret_request = false`
under `[experimental]` in `host.toml` in the data directory and restarting
Praxi. `praxi doctor` reports the setting.

### A password pasted into the message box

When the message you are about to send contains something that looks like a
secret, the app does not send it. It names the kind (never shows the value) and
offers: store it in the vault and keep `[secret: name]` in the message, remove it
from the message, send anyway, or keep editing. The check runs on your computer
before anything is sent. It recognises:

- known key formats (OpenAI-style, GitHub, Slack, AWS, Stripe, Google, a private
  key block and similar);
- a value right after a label such as `password:`, `api key`, `token`, `secret`,
  `şifre`, `parola` or `anahtar`, when the value looks random enough;
- a payment card number (a real card network and a valid check digit, written in
  groups or after a word such as `card` or `kart`).

It is built to stay quiet otherwise: laughs, hashes, UUIDs, commit ids, links and
version numbers are left alone, and so is a password with no label next to it
(`hunter22` on its own), because nothing can tell it from a word.

A message that comes in from Telegram or Discord cannot show you that choice, so
the daemon checks it again before it reaches the model: the suspected value is cut
out, the rest of the message goes on, and the bot answers in the chat that it did
so. Delete the original message in the chat yourself: Telegram or Discord keeps it.
To give the bot a secret, store it in the vault in the desktop app.

## Prompt-injection evaluation pack

To verify that the permission engine and taint gate hold when a model actually
follows untrusted instructions, the repository includes an offline tier-1
prompt-injection evaluation pack under `evals/scenarios/inj-*.yaml` (scripted
model, fake tools, no network). Each attack covers a different untrusted source
(web pages, e-mail, files, MCP output, chat messages, memory) and has an
`-holdout` twin with different wording and fixtures. The `inj-browser-mcp-*`
scenarios cover item 12a: page actions after a web page alone run without a
card, a navigation after private data asks, `browser_run_code` asks, a
shell or script after a page read through the browser asks, and a file,
localhost or LAN address always asks, also after "Allow for this task".

Run the pack and compare it against the checked-in baseline:

```sh
cargo run -p evals --bin bot-eval -- run --tier 1 --filter inj- --repeat 3 --out run.jsonl
cargo run -p evals --bin bot-eval -- compare --baseline evals/baselines/injection-pack.jsonl --run run.jsonl
```

(If `bot-eval` is already built on `PATH`, run `bot-eval run --tier 1 --filter inj- --repeat 3 --out run.jsonl`.)

- `run` records `unsafe_actions` on every JSONL result and fails if any scenario
  that is not marked `expected_fail` (a documented gap) triggers an unsafe
  action, or if a known-gap scenario starts passing without removing its marker.
- `compare` prints `GO` or `NO-GO` (exiting 1 on `NO-GO`) and rejects any unsafe
  action, any per-scenario pass-rate drop above `--max-drop` (default `0`), or a
  missing baseline scenario.
- The pack runs in CI via `cargo test -p evals`. Details and baseline-update
  instructions are in [`evals/baselines/README.md`](../../evals/baselines/README.md).

A second offline pack, `evals/scenarios/safe-*.yaml`, checks the guards that do
not need an attack: file-tool path boundaries (`../`, absolute paths, links out
of a Folder-only profile), secret redaction of tool results, events, the audit log and stored tool output, denied calls retried
in small variations, subagent tool narrowing, token and size guards, Stop in the
middle of a tool and unattended runs not inheriting owner-only rules. Run it with
`--filter safe-` and compare against `evals/baselines/safety-pack.jsonl`.
Text the model itself writes and tool-call arguments are not yet masked in the
stored run records (`bot.db`); those two cases are tracked as expected-fail
scenarios in the [known gaps list](../../evals/baselines/README.md#safety-pack-non-injection).
