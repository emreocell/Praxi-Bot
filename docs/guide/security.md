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

## What it does not protect you from

Be realistic about the limits:

- **Your model provider sees your prompts and tool results.** A cloud provider
  receives whatever the bot reads and works with, including file contents and page
  text. Use a local model (Ollama, LM Studio) for sensitive material. See the
  [Gemini free tier](providers-and-models.md#gemini-free-tier) note.
- **An approval is only as good as your reading of it.** A request can look
  harmless and still matter. Read the summary; deny when unsure.
- **"Always allow" and the `full` preset are standing permissions.** A bot with
  broad grants that reads a malicious page can act within them. Prefer
  per-category rules, narrow `[tools]` lists and separate bots for risky jobs.
- **The never-list is a safety net, not a sandbox.** It recognises known
  catastrophic patterns; it cannot judge every harmful command. Shell access
  (`shell_run`) is as dangerous as giving someone a terminal.
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
effort and retain pause. A legacy release request cannot release an owned or
restored uncertain latch. Resume requires explicit owned release or privileged
recovery, durable safe continuation notes and successful reset. Audit stores only
actor/target IDs, event-type counts, sequence ranges, timing and fixed outcomes;
audit failure denies authorization and suspends input. See the
[live-view API and integration status](../api/live-view.md).
