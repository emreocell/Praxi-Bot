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

The disk token has only CLI authority (status, lists, chat and run cancellation /
resumption). Management, approval/question decisions, account configuration and
all screen control routes require a per-launch in-memory management bearer.
The desktop generates it, writes it to its daemon's anonymous stdin pipe and
uses it only in HTTP headers. It is never loaded from a file or environment
variable, and is not accepted as an events URL token. Paired peers still use
their transport identities; Telegram keeps its in-process gateway credential.
See [ADR-0030](../kararlar/0030-yerel-api-yetki-ayrimi.md).

**This does not sandbox arbitrary same-user code.** A shell can still modify
configuration/database/peer-key files directly, invoke offline CLI commands,
automate the UI or (where OS policy permits) read daemon/UI process memory.
Existing process groups/jobs manage lifetimes; they are not a memory or file
security boundary. Do not approve untrusted shell/MCP/CLI-engine programs on
the assumption that the credential split isolates the whole computer.

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

## Process memory hardening and compatibility

Before reading its launch credential, the daemon protects its process object.
On Windows, an OWNER RIGHTS DACL blocks new same-user opens for process memory,
handle duplication and ACL changes; ordinary query/wait/termination still work.
The desktop applies the same guard before generating credentials. This does not
protect thread objects, revoke already-open handles, or stop DLL/code injection,
UI automation or direct configuration writes. It is not a restricted-token or
AppContainer sandbox. Administrator/debug privileges remain outside this guard.

On Linux, credential holders become non-dumpable. The daemon also sets
`no_new_privs` before creating workers, so shell, MCP and CLI-engine descendants
inherit it. Existing jobs/process groups still manage process lifetimes. There
is no separate UID, bwrap or Landlock filesystem boundary yet.

For compatibility, `<data dir>/isolation.toml` accepts `memory_guard = false`
and/or `linux_no_new_privs = false`; both default to true and malformed policy
fails startup. Disabling memory protection logs a warning. No-new-privs prevents
sudo/setuid/file-capability elevation and may affect older browser sandboxes.
Process memory guards can prevent debugger/crash-dump attachment. Linux runtime
and packaged Tauri compatibility still require validation before release.

## Checking the running daemon

`bot doctor` reads the authenticated `/v1/isolation` report with the low-privilege
token. It reports the privilege split, child environment scrubbing, observed
process memory protection, inherited no-new-privs, and the absence of a full
Praxi child sandbox separately. Missing fields or an unavailable/older daemon
are shown as unverified. A token file or a recent version alone proves nothing.

File tools share PR #30's data-directory protection helper for authority-bearing
state/configuration, database sidecars and private directories. `isolation.toml`
is added to that same list. This includes canonical Windows trailing-dot/space
and alternate-stream names. Normal project files outside the daemon data
directory retain their usual behavior. These checks do not restrict arbitrary
shell programs or defend against hard-link aliases created outside the tools.
