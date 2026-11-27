# Coding CLI engines

Settings → Providers lists CLI engines on the **daemon's computer**. Refresh
re-runs bounded version and authentication probes. Detection also warms at
daemon startup. Terminal PATH entries are supplemented with user-local, npm,
Node version-manager and native Windows installation directories. Relative
PATH entries, the working directory and links back into it are refused.
Resolved shim programs and scripts are checked again against that boundary.
An unrelated `agent` on PATH cannot hide Cursor's vendor-directory alias.

| Engine | Commands (Windows uses PATHEXT) | Auth evidence | Install command, all OS unless specified | Brain |
| --- | --- | --- | --- | --- |
| Claude Code | `claude` | `auth status --json` | `npm install -g @anthropic-ai/claude-code` (verified local reference) | Yes, minimum 2.1.257 plus runtime capability checks |
| Codex | `codex` | `login status` | `npm install -g @openai/codex` (verified local reference) | Blocked: `unified_exec` cannot be disabled |
| Gemini CLI | `gemini` | Credential-file metadata only; contents never opened | `npm install -g @google/gemini-cli` (**unverified**, copy only) | Detection only; minimum unverified |
| Antigravity | `agy` | None: no status command (sign-in shows at the first turn); on a server the token folder `~/.gemini/antigravity-cli/` (existence and age only) confirms a sign-in | none known | Runs with its own tools (`agy acp`); models from `agy models` |
| Qwen Code | `qwen` | Credential-file metadata only (`~/.qwen/oauth_creds.json`); contents never opened | `npm install -g @qwen-code/qwen-code` (verified against the vendor source 2026-11-26; the Install button runs it after your approval) | Runs with its own tools (`qwen --acp`, experimental); minimum unverified |
| Kiro CLI | `kiro-cli` | None: `whoami` exit codes are undocumented; `KIRO_API_KEY` is not evidence | Unix: `curl -fsSL https://cli.kiro.dev/install \| bash` (verified against the vendor source 2026-11-26; the Install button runs it after your approval) | Runs with its own tools (`kiro-cli acp --agent-engine v3`, experimental); minimum unverified |
| Qoder CLI | `qodercli` | `status -o json` (`logged_in`) | `npm install -g @qoder-ai/qodercli` (verified against the vendor source 2026-11-26; the Install button runs it after your approval) | Runs with its own tools (`qodercli --acp`, experimental); minimum unverified |
| GitHub Copilot CLI | `copilot` | None: no status command (sign-in is `copilot login`, kept in the OS credential store; a GitHub token in the environment is not evidence) | `npm install -g @github/copilot` (verified against the vendor source 2026-11-26; the Install button runs it after your approval) | Runs with its own tools (`copilot --acp`, experimental); minimum unverified |
| Kimi Code CLI | `kimi` | Credential-file metadata only (`~/.kimi-code/credentials/kimi-code.json`); contents never opened | `npm install -g @moonshot-ai/kimi-code` (verified against the vendor source 2026-11-26; the Install button runs it after your approval) | Runs with its own tools (`kimi acp`, experimental); minimum unverified. The Python `kimi-cli` 1.x is its retired predecessor |
| Mistral Vibe | `vibe-acp` | None: no status command; the API key can be in the environment, `~/.vibe/.env`, the OS keyring or a browser sign-in, and none of them is evidence | `uv tool install mistral-vibe` (**unverified**, copy only) | Runs with its own tools (`vibe-acp`, experimental); minimum unverified. Runs the CLI's own model: Vibe saves a model chosen over ACP as the user's default, so another model is refused |
| Goose | `goose` (the database migration tool of the same name, which prints `goose version: ...`, is reported as another program) | None: no status command; keys are in the OS keyring or `secrets.yaml` (set up with `goose configure`), and none of them is evidence | Unix: `curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh \| CONFIGURE=false bash` (**unverified**, copy only) | Runs with its own tools (`goose acp`, experimental); minimum unverified. Without a configured provider the run ends as signed out |
| Cline | `cline` | None: no status command is documented (sign-in is `cline auth`; keys are in `~/.cline`, which Praxi's file tools never open) | `npm install -g cline` (**unverified**, copy only) | Runs with its own tools (`cline --acp`, experimental, data entry `providers/cli/cline.toml`); minimum unverified. Runs the CLI's own model |
| Augment Code | `auggie` | None: `auggie auth status` has no documented output, so sign-in is never claimed (sign-in is `auggie auth login`) | Unix: `npm install -g @augmentcode/auggie` (**unverified**, copy only). The install page lists macOS, Linux and WSL, not native Windows | Runs with its own tools (`auggie --acp`, experimental, data entry `providers/cli/auggie.toml`); only at Full access, see below. Runs the CLI's own model |
| OpenCode | `opencode` | Credential-file metadata only; contents never opened | `npm install -g opencode-ai` (verified local reference) | Detect/install only: inspected CLI has no switch to disable native tools |
| Cursor | `cursor-agent`; `agent` only inside a `cursor-agent` directory | `status --format json`, then `status` | Windows: `irm 'https://cursor.com/install?win32=true' \| iex`; Unix: `curl https://cursor.com/install -fsS \| bash` (verified local reference) | Detection only; minimum unverified |
| ChatGPT plan | Same Codex executable | Unknown; no Praxi PKCE flow | Use the Codex row | Detection only |

"Verified" means the command appears in a local reference. No installer or
real model prompt was run while implementing this feature. Minimum versions
are unknown unless listed above. Missing credential files do not prove that
an engine cannot use an OS keychain; the UI distinguishes unknown status.
Ambient API keys are withheld and are not evidence of a usable CLI login.

## Install and sign in on a server

On a headless server (a paired host) the engine list under Settings → Home shows each coding CLI **as it is installed there**: the daemon on the server answers, as its own service user, so the list always matches what the bots on that server can run. **Install on server** runs the engine's fixed installer as that user (an npm package into a user-level prefix, or the vendor's installer script into the user's `~/.local/bin`); nothing is written outside that user's own folders. The row then reads *Ready · account* (when the CLI names its account), *Installed · sign-in needed* or *Installed · sign-in state unknown*.

**Sign in on server** runs the vendor's own login command on the server and shows what it asks of you in the app: a link with an **Open the page** and a **Copy link** button, the one-time code to type there (device flows), or a box to paste the code the page shows (paste flows). When it finishes, the server confirms the sign-in and the app shows the account. The login runs on a terminal (a PTY), because these CLIs only print their link when they think a person is typing. Only the link, the code and the account name leave the server; the rest of the output stays private and a pasted code goes straight to the CLI and is never kept. The link is the first one the CLI prints, and only when its host is exactly one of the vendor's sign-in hosts (no subdomains: not `*.amazonaws.com`, not `google.com`) and its path is the one expected; the window shows that host above the buttons. A sign-in is confirmed by the CLI's own status, by a success line ("not logged in" is not one), or by a credential file that appeared or changed after the link was shown (never a symlink, never a folder). If the desktop loses host administration while a sign-in waits, the sign-in stops and its process tree is killed.

**Which user.** All of this runs as the user the Praxi service runs as (`praxi` on an installer-managed server), in that user's home folder. The engine list says so at its top and shows where each CLI was found. A CLI installed over SSH as `ubuntu` is a different install and is not seen; use the app, the in-app terminal, or `sudo praxi cli install <engine>` / `sudo praxi cli login <engine>` / `sudo praxi cli status` (they switch to the service user; see [Coding CLIs on a server](remote-host.md#coding-clis-on-a-server-claude-code-codex-antigravity-)). Engines without a flow below (Qwen Code, GitHub Copilot CLI, Mistral Vibe, Goose) are signed in by hand with the same commands, because their sign-in cannot be driven from a window (Vibe asks for an API key; the others are not verified here).

| Engine | Command (on the server) | What the app shows | Confirmed by |
| --- | --- | --- | --- |
| Claude Code | `claude auth login` | Link, then paste the code | `claude auth status --json` (account) |
| Codex | `codex login --device-auth` | Link and one-time code | `codex login status` |
| Cursor | `cursor-agent login` (`NO_OPEN_BROWSER=1`) | Link only | `cursor-agent status` |
| Antigravity | `agy` (its first run signs in) | Google link, then paste the code | The token folder it writes |
| OpenCode | `opencode auth login opencode` (login method: the console account) | Link (and the code, when it prints one) | `auth.json` written during the sign-in |
| Kiro | `kiro-cli login --license free --use-device-flow` | Link and one-time code | The success line and exit 0 |
| Qoder | `qodercli login` | Link only | `qodercli status -o json`, or "Login successful! Welcome, name." |
| Kimi | `kimi login` | Link and one-time code | "Logged in to ..." and exit 0 |

The Kimi and Qoder rows follow the text of those CLIs' own source (npm 2.8.1 and 1.1.99); OpenCode's menu was read on a terminal (opencode 2.3.0); Cursor, Kiro and Antigravity follow the vendors' documentation and were not run signed in. When a CLI does not show the expected link in time, the window says so and offers the plain command, to run over SSH or to ask your bot to run (the bot can answer a CLI's prompts with the `shell_interactive` tool, see [Agent tools](agent-tools.md)). A sign-in that is already working is never replaced from here.

Install shows the exact fixed command and an expiring **Approve once / Deny**
card. It runs only after the authenticated local owner approves. No standing
grant, bot policy, tool call, schedule or paired-device role can approve it.
All peer roles are refused before route dispatch, including encoded and
slash-normalized variants. The installer is not an agent tool. The local
bearer credential is therefore owner authority and must remain private.

`POST /v1/providers/cli/install` prepares the approval. The separate
`POST /v1/providers/cli/install/{approval_id}/decision` consumes it once.
The response streams `output` and `done` SSE events. Installer text is
untrusted and is rendered as text, never Markdown, HTML or commands.
Audit records precede spawning. One installer runs at a time, for at most ten
minutes, with a 64 KiB output limit and no requested elevation. Disconnecting
the stream or shutting down the daemon terminates the owned process tree;
version/auth detection is refreshed afterwards. Installation uses the
vendor's global command as the current user: system-owned npm prefixes can
fail with a permission error; Praxi does not retry with administrator rights.

Windows npm is launched as `node.exe <canonical npm-cli.js>`; no `.cmd` is
passed to `cmd.exe`. Cursor uses its fixed vendor PowerShell command on
Windows and the fixed vendor shell command on Unix. These strings come from
the registry; no request/model value is interpolated. Unverified commands
cannot reach this executor. Some npm JavaScript shims remain refused by
normal engine detection; those entries report `shim_refused` rather than
claiming a verified version or usable brain.

Sign in separately in your terminal using the command shown in Settings.
Praxi does not log in for you, read/copy tokens, or turn CLI credentials into
API keys. Quota windows remain unknown; existing quota errors still stop
turns. Check the vendor's subscription terms for your intended use.

After enabling a detected Claude engine, select `cli:claude` in a bot's
executor model picker and choose `default`, `sonnet` or `opus`.
Model access is determined by the CLI account. CLI engines are excluded
from classifier/planner selection as before. No new Tauri invocation is used.

## Security boundary

Claude continues to run with `--tools ""`, strict MCP configuration, isolated
settings and runtime tool checks. Codex's existing read-only sandbox does not
disable every native tool and remains blocked. CLI configuration directories
(including hooks and MCP settings), credential files and home overrides are
protected by file tools, including `.opencode`, `opencode.json`/`opencode.jsonc`,
`OPENCODE_CONFIG_DIR` and the file named by `OPENCODE_CONFIG`.
Child environments use an explicit allowlist of OS
essentials and CLI home paths. Praxi secrets, provider keys, proxy credentials
and code-injection variables such as `NODE_OPTIONS` are not inherited.

OpenCode is deliberately unavailable as a bot brain. Installed OpenCode
**v2.2.5** `--help`, `run --help`, `acp --help`, and `auth --help` were inspected
without a login or model prompt. Headless `run --format json`, model
`provider/model#variant`, `--session` and stdio `acp` are advertised, but no
verified native-tool-disable facility is advertised. There is no existing
delegated coding-tool integration to expose it through instead.

`cli-provider/src/opencode.rs` contains experimental ACP request construction
and a fail-closed frame decoder, disconnected from process execution. It
preserves exact model IDs, sessions, text/thought separation and completion,
and rejects native-tool events/requests. Its transcript is **synthetic**,
based on local protocol references; it is not a recording of a live session.
Claude `--help` also confirmed `sonnet`/`opus` aliases, `--tools ""`,
`--setting-sources` and `--strict-mcp-config` without sending a prompt.

Enabling OpenCode execution requires vendor-source confirmation of confinement and
recorded, credential-free compatibility fixtures for the installed CLI.

## Local evidence and attribution

The implementation is original Rust/React code. Design ideas were learned
from [OpenMausBot](https://github.com/milind-soni/OpenMausBot) (local checkout,
Apache-2.0 LICENSE inspected; no source code copied):

- `docs/cli-onboarding.md`, `docs/custom-engines.md`, `docs/opencode-go.md`,
  `docs/cursor.md`, `docs/headless-cli.md`: onboarding and CLI/ACP semantics.
- `server/env-path.ts`, `server/cli-probe.ts`, `server/harness/registry.ts`:
  GUI path augmentation, safe probes and snapshots.
- `server/engine-install.ts`, `server/cli-setup.ts`: fixed npm arguments,
  confirmation, timeout, bounded output and re-detection.
- `server/drivers/claude.ts`, `server/drivers/codex.ts`,
  `server/drivers/acp/cursor.ts`, `server/drivers/acp/opencode-go.ts`:
  install and login descriptors.
- `server/drivers/acp/core.ts`: session/new/load, set_config_option,
  session/prompt, session/update and stopReason.

Praxi PR #97's branch `fix/c244a-mcp-start-and-tool-availability`,
`crates/mcp-bridge/src/transport.rs`, was also inspected: it resolves the
parent's executable before clearing the child environment and preserves only
explicit OS essentials. CLI discovery adds deterministic GUI install paths
without executing login-shell startup scripts.

## Running a CLI with its own tools (engine mode)

Each bot has an engine mode, set by the local owner only (bot settings, "Engine"):

- **Praxi-governed** (default): Praxi runs files and shell with its own tools and
  permission engine; the CLI's native tools are off (Claude Code only today).
- **Native tools**: the CLI keeps its own file and shell tools in a workspace folder
  you choose. The bot's approval level maps to the CLI's own permission mode, and
  every permission request the CLI forwards becomes a normal Praxi approval card.
  Praxi's own tools (browser, apps, secrets, memory) are offered to the CLI over a
  per-run local MCP endpoint and keep Praxi's approvals.

The default level is **Ask**. **Full access** needs its own consent per bot. Switching
a bot to native tools needs a consent too; both are recorded in the audit log.

Praxi makes the work folder for the bot, so picking a model never asks for one:
`<Documents>/Praxi/<bot>` on a desktop and `/var/lib/praxi-work/<bot>` on an always-on
host (outside Praxi's data folder, where the folder rule below accepts it). Choose a
folder yourself only when you want a specific one.

A bot on **Full access** asks nothing except before spending money, so when you pick a
CLI that only works with its own tools (every one in the table except Claude Code and
Codex, which can also run confined) for a Full bot, the app switches native tools on at
the Full level without a dialog. Your pick of the engine is the consent: it goes through
the owner-only engine setting and the audit log records it like any other change. A turn,
the model or another caller never switches it on; a bot left on such a CLI without it is
refused with a plain message, and the app applies your pick again at the next message you
send from it. The CLI's own tools do not go through Praxi's money card; Praxi's tools
(browser, apps, memory) still do. If the CLI is not installed, signed out or out of date
on that computer, the composer says so before sending and offers Install (Settings, AI)
or another model, and nothing is switched on for an engine that cannot start.

| CLI | Ask | Accept edits | Auto | Full access |
| --- | --- | --- | --- | --- |
| Claude Code | `--permission-mode default` (`manual` on newer builds) | `acceptEdits` | `auto` | `bypassPermissions` |
| Codex | `on-request` + `workspace-write` | runs as Ask | + `auto_review` reviewer | `never` + `danger-full-access` |
| Cursor (`acp`) | default | runs as Ask | `--auto-review` | `--force` |
| OpenCode (`acp`) | every request asks | runs as Ask | runs as Ask | requests approved automatically |
| Gemini CLI (`--acp`, experimental) | default | `--approval-mode auto_edit` | runs as Ask | `--yolo` |
| Antigravity (`agy acp`, experimental) | `default` mode | `accept-edits` mode | `auto` mode; Full: `--dangerously-skip-permissions` |
| Qwen Code (`--acp`, experimental) | `--approval-mode default` | `--approval-mode auto-edit` | `--approval-mode auto` | `--approval-mode yolo` |
| Kiro CLI (`acp`, experimental) | default permissions | runs as Ask | runs as Ask | requests approved automatically |
| Qoder CLI (`--acp`, experimental) | mode `default` | mode `acceptEdits` | mode `auto` | mode `yolo` |
| GitHub Copilot CLI (`--acp`, experimental) | default | `--allow-tool write` | runs as Ask | requests approved automatically (never `--allow-all-tools`) |
| Kimi Code CLI (`kimi acp`, experimental) | Windows: mode `default` (Always Ask). Linux and macOS: not offered, because that mode approves edits inside a git repository | runs as Ask | mode `yolo` (Ask When Needed) | mode `auto` (Never Ask) |
| Mistral Vibe (`vibe-acp`, experimental) | mode `ask` | mode `accept-edits` | mode `smart-approve`; refused when the session does not offer it (it is behind a rollout gate) | mode `auto-approve`; refused when the session does not offer it (`enable_auto_approve = false`) |
| Goose (`goose acp`, experimental) | mode `approve` | runs as Ask | mode `smart_approve` | mode `auto` |
| Cline (`--acp`, experimental) | `--auto-approve false` | runs as Ask | runs as Ask | `--auto-approve true` |
| Augment Code (`--acp`, experimental) | not offered | not offered | not offered | requests approved automatically |

A level a CLI has no mode for runs as Ask, never as something wider. When an ACP CLI
lists its modes in `session/new` and the level's mode is not among them, the turn is
refused before the prompt is sent, because a CLI may accept an unknown mode silently. The workspace
cannot be a drive root, your home folder, Praxi's data folder or a credential or CLI
settings folder. The CLI runs with an allowlisted environment (no Praxi secrets, no
provider keys) and Stop or the per-bot kill switch ends its whole process tree.

Goose starts a new session in `auto` ("Automatically approve tool calls") unless the
user's config says otherwise, so Praxi always sends the level's mode; it applies to that
session only. Three things to know about Goose:

- **Auto asks a model.** In `smart_approve`, calls the MCP server marks read-only run
  without asking. Goose's own model judges the rest (`permission_judge`), which can add
  model calls. Calls it judges not read-only ask, and that decision is cached in
  `permission.yaml`.
- **Saved rules still apply.** A tool the user marked `always_allow` in `permission.yaml`
  runs without asking, in every mode. Praxi answers permission requests with the
  one-time options, never "always".
- **Network at startup.** The released binary downloads its model catalog from
  `https://models.dev/api.json` on every start. Only a build without that feature turns
  it off.

A model chosen in Praxi is sent as the session's `model` config option, which Goose keeps
on that session and does not save as the user's default.

### Engines defined by a data file

Cline and Augment Code are not Rust code: each is one file in `providers/cli/` (`cline.toml`,
`auggie.toml`), read and validated by `cli_provider::acp_data` and run by the same launcher,
approval cards, consent steps and checks as the built-in engines. A file states the command,
the fixed ACP arguments, the install package, the sign-in text to copy, which approval level
maps to which flag or mode, how a model is chosen, the folders that hold the CLI's
credentials, what was read and where, and a list of facts it does **not** claim (`unverified`).

What a file can never do: hold a command line (only a bare executable name and fixed
arguments, with no placeholder for a prompt, a model or any user text), install anything but
`npm install -g <package>`, widen a level (a level maps to the same or a narrower one, and
one the CLI cannot be made to keep is refused before anything starts), put a permissive flag
(`--yolo`, `--auto-approve`, `--force`, ...) below Full access (a flag may only pin the safe
value, as in `--auto-approve false`), be anything but experimental, or reuse the id or the
command of another engine. A file that breaks a rule is dropped, and the build's tests fail.
The engine folder rule, the consent steps for native tools and Full access, and the money gate
apply exactly as for the built-in engines, because they belong to the engine mode, not to one
engine.

- **Cline** (`cline --acp`): the docs say nothing is auto-approved in ACP mode and give a
  boolean `--auto-approve`, which Praxi always passes (`false` below Full access, `true` at
  Full access) so a setting of the user cannot make a lower level approve. No edits-only or
  reviewer mode is documented: Accept edits and Auto run as Ask.
- **Augment Code** (`auggie --acp`): the ACP pages do not say whether permission requests
  are forwarded or tools run without asking, and the permissions page only says the CLI
  follows its implicit runtime behavior when no rule matches. Praxi cannot make it ask, so
  only Full access is offered (the rule OpenCode follows).
- Neither engine is sent a model: the docs name no stable ACP way to choose one, so a model
  other than `default` is refused.

Looked at and **not added**: Factory Droid (the docs give `droid exec --output-format acp`,
the ACP registry `acp-daemon`, and nothing explains the difference), Junie (the public docs
index was empty; the launch flag exists only in a staging registry file), Amp (no official
ACP mode; the registry lists a third-party wrapper, `amp-acp`, and Amp's own headless mode
speaks a different protocol), Grok Build (no reachable official docs) and Crush (no confirmed
ACP mode). Each can become a data file once its launch command and permission behaviour are
verified against the vendor's docs or a real install.
