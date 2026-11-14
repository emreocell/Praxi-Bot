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
with `icacls` on Windows; the daemon warns if that fails). The one route without
the token, `POST /v1/webhooks/{id}`, requires a valid HMAC signature.

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
you have a standing grant for it. That includes desktop actions that type, click,
or launch something, browser scrolling and tab changes, and mailbox reads,
searches, and mark-read: those calls send the model's arguments to another
machine. A search that can only reach the search provider you configured stays
prompt-free after untrusted content, until the run has also read something
private. Later turns of the same chat stay gated while a message that still
carries that content is in the prompt. The only way to skip this ask is an
explicit per-bot setting, "allow sending data after this bot has read something",
which you turn on in the bot form. An empty tool allow list is not a bypass.
Deny rules and the floors in item 3 still win. A run that has not read anything
is unchanged, and existing grants still apply to it. Approving one send does not
create a standing grant. The card lists every recipient and every field that
will be sent. Email attachments are covered by `email_read`; there is no
separate attachment tool.

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
a takeover latch restored after a restart is cleared by the local user with
`POST /v1/control/recover`, which needs no view session and no registered
target. Resume otherwise requires explicit owned release or privileged
recovery, durable safe continuation notes and successful reset.

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
control; the pause itself stays.

The continuation note a takeover leaves in the chat is the only message the
model receives as a system message from stored history. It has its own role that
only the daemon writes and exactly one fixed text shape (intervention type, event
counts, target kind); a stored message with any other text or role is never
delivered as a system message, and a state-bundle import turns such rows (and any
unknown role) into the inert `imported_note` role, which is never sent to the
model; history forwards only an allow-list of roles (user, assistant, the genuine
control note) and ignores every other value. The daemon's own files (`bot.db`, `schedules.db`, `memory.db` with their
WAL/SHM/journal, `control-latch.json`, `policy.json`, `host.toml`, `config.toml`,
`mcp.toml`, `providers.toml`, `web.toml`, the Telegram state files, the
`peer`, `bots`, `profiles` and `runs` folders and the bearer token) are protected
from the bot's file tools for reading and writing, including names that differ
only by a trailing dot or space or by a `:stream` suffix on Windows; skills and
workflows stay writable because the bot authors them, and a shell command that names them
always asks for approval. That last rule is damping only: one approved
`shell_run` still runs as you and can reach those files and the loopback API
(see below). Audit stores only
actor/target IDs, event-type counts, sequence ranges, timing and fixed outcomes;
audit failure denies authorization and suspends input. See the
[live-view API and integration status](../api/live-view.md).
