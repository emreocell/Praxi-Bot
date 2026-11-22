# Bots and profiles

A **bot** is a named assistant with its own persona, model choice, tool set and
permission rules. You can have many: for example a cautious "Researcher" that
only browses, and a "Operator" that may touch files. Each bot has its own chats
and its own "always allow" grants.

A bot is defined by a **profile**: a small TOML file (or the same structure as
JSON).

## Create a bot

```sh
bot bots add mira.toml
# bot created: 3f1c... (Mira)
bot bots list
```

`bot bots add` sends the file to the running daemon (`bot serve` must be up).
The desktop app has a bot form that writes the same structure.

## A complete profile

```toml
[bot]
id = "mira"                       # slug
name = "Mira"                     # display name
persona = "You are Mira, a careful research assistant. Cite your sources."
language = "en"                   # optional preferred language code

[models]
executor   = "gemini/gemini-2.5-flash-lite"   # required: the model that runs the loop
planner    = "gemini/gemini-3.1-pro-preview"  # optional, informational for now
classifier = "gemini/gemini-2.5-flash-lite"   # optional: fast routing model (K3)
fallback   = ["ollama/llama3.2"]              # optional: alternate models on failure
# light    = "provider/your-light-model"     # optional: opt-in model tiering

[permissions]
preset = "approve-destructive"    # read-only | read-and-draft | approve-destructive | full

[permissions.categories.shell.level]   # optional per-category overrides
level = "deny"

[permissions.categories.fs.level]
level = "allow"

[tools]
allow   = ["fs_*", "browser_*"]   # empty = every tool
exclude = ["fs_delete"]           # always removed, even if allowed

[router]
enabled = true                    # run the decision ladder before the main model
k3 = true                         # use the classifier model (default on if one is set)
```

Only `[bot]` (with `id` and `name`) and `[models]` with an `executor` are
required. Everything else is optional.

### Model references

A model is written `provider/model`, where the provider is the id you configured
(see [Providers and models](providers-and-models.md)), for example
`gemini/gemini-2.5-flash-lite` or `anthropic/<model-id>`.

The `planner` field is informational. The `classifier` is used by the decision
ladder when `[router]` allows it. The loop starts with `executor` and tries
`fallback` models after retryable errors or missing capabilities. An optional
`light` model enables [model tiering](model-tiering.md) for simple first turns;
all setup presets leave it unset.

### Permissions in a profile

- Bots created by `bot setup` with the General preset get `mcp:*` in
  `[tools] allow`, so they can reach MCP servers. This only makes the tools
  visible: MCP tools still ask for approval under the default policy. Profiles
  saved earlier are not changed; add `"mcp:*"` to a bot's `[tools] allow` list
  to give it the same reach. Which servers a bot may use is set on the server
  (`bots`), see the MCP servers guide.
- Email is not on by default. The `email_list_folders`, `email_search` and
  `email_read` tools only read, so they run without a prompt, and together with
  `web_*` a malicious page could make the bot read your mailbox into a web
  search. To opt in a bot, add `"email_*"` to its `[tools] allow` list (sending
  with `email_send` still asks) and keep that bot's web tools off or on ask.
- `preset` picks a starting policy. The four presets are explained in
  [Chatting and approvals](chatting-and-approvals.md#permission-levels).
- `[permissions.categories.<name>.level]` then overrides single categories
  (note the doubled `level`: the table is called `level` and holds a key
  `level`). A level is `deny`, `ask`, `allow`, or `allow_with_limits`
  (intended as allowed within limits such as an hourly cap or path patterns;
  in the current resolver it behaves like `allow`).
  Category names: `fs`, `shell`, `desktop`, `browser`, `accounts`, `messaging`,
  `money`, `system`, `remote`, `script` (and the internal `meta`, always
  allowed).
- A rule can also differ by who started the run, using `by_initiator`
  (initiator kinds: `human`, `schedule`, `routine`, `channel`, `subagent`; for
  example `[permissions.categories.fs.by_initiator.schedule]` with
  `level = "deny"`). A scheduled run follows the `human` setting unless you give
  `schedule` its own.
- If a profile has no `[permissions]` at all, the **global default policy**
  applies (initially "Approve destructive"). That policy can be changed through
  the API (`GET`/`PUT /v1/policy`) and the desktop app.

### Tool filters

Tool names are the built-in names, for example `fs_read`, `fs_write`,
`shell_run`, `browser_open`, `desktop_click`, `script_run`. Use `*` and `?`
wildcards. The built-in tool groups:

| Group | Tools |
| --- | --- |
| Files | `fs_read`, `fs_list`, `fs_search`, `fs_write`, `fs_edit`, `fs_move`, `fs_delete`, `checkpoint_list`, `checkpoint_restore` |
| Shell | `shell_run` |
| Browser | `browser_open`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_select`, `browser_press`, `browser_scroll`, `browser_wait`, `browser_back`, `browser_tabs`, `browser_screenshot`, `browser_eval` |
| Web (read-only, no browser) | `web_search`, `web_extract`, `web_feed`, `web_sitemap`, `web_metadata` (see [Web tools](web.md)) |
| Schedules (General preset only) | `schedule_list`, `schedule_create`, `schedule_update`, `schedule_pause`, `schedule_resume`, `schedule_delete` (see [Scheduled tasks](scheduled-tasks.md)) |
| Desktop (Windows) | `desktop_list_apps`, `desktop_launch`, `desktop_get_state`, `desktop_click`, `desktop_type`, `desktop_set_value`, `desktop_key`, `desktop_scroll`, `desktop_drag` |
| Scripts | `script_run` |
| Agent-internal | `ask_user`, `todo_write`, `spawn_subagent` |
| Skills and workflows | `skill_list`, `skill_read`, `skill_write`, `skill_hint`, `workflow_list`, `workflow_run` |

When many tools are registered (more than 40) the model sees a small discovery
interface (`find_tool`, `get_tool_details`, `use_tool`) instead of the whole list.

## Managing bots

| Task | How |
| --- | --- |
| List bots | `bot bots list` |
| Inspect one, including its effective policy | `GET /v1/bots/{id}` (API; desktop app) |
| Replace a profile | `PUT /v1/bots/{id}` with the `expected_version` from the profile being replaced; missing precondition is `428 precondition_required`, stale is `409 version_conflict` with the current bot. Legacy callers must reload and explicitly retry; the server never upgrades a stale full document automatically |
| Change single fields safely | `PATCH /v1/bots/{id}` with `expected_version` and `ops` (`set` / `unset` on JSON Pointer paths such as `/models/planner` or `/permissions/categories/fs/level`); a stale version is a `409` that returns the current bot. `GET` and list return profile + `version` in one storage snapshot. Successful profile writes and skill assignment changes advance the same bot revision in their write transaction. The edit modal saves with this. At most 128 operations, 32 path segments / resulting JSON levels, 4096 path bytes and a conservative 1 MiB escaped JSON budget; excessive input is refused before applying it |
| Delete a bot | `DELETE /v1/bots/{id}`; only possible while it has no chats or runs |
| See or revoke its "always allow" grants | `bot grants list <bot-id>`, `bot grants revoke ...` |

The CLI has no command for editing or deleting bots yet; use the desktop app or
the HTTP API (see `docs/api/openapi.json`).

### Main bot and bot order (desktop app)

On a fresh desktop setup, the first-run wizard defaults to creating a bot named
**Praxi** with the white ghost avatar. It starts first in the left rail as your
**main bot** (the default bot when the app opens or starts a new chat).

Each computer keeps its own separate bot order and main bot—**This computer**
and each paired host store their own list locally in desktop settings, so
switching computers or changing the main bot on a paired host never changes the
main bot on any other computer.

When you have more than one bot on the selected computer:

- To make another bot the main bot in one click, open its right-hand **bot
  panel** (click the bot chip in the title bar) and press the star button
  (**Make main bot**) next to the bot's name in the panel header. It moves to
  the top of that computer's list and keeps every other bot in order.
- The computer card in the bot panel also shows the current main bot with a star
  icon alongside its status (**Online**, **Working**, or **Offline**).
- You can also drag a bot's grip handle in the left rail (or focus the handle
  and press the Up/Down arrow keys) to reorder that computer's list.

### Bot avatars (desktop app)

In the desktop app (**New bot**, **Edit bot**, or the first-run wizard), each
bot can have either:

- **One of 16 procedural vector characters** (`pebble`, `cube`, `diamond`,
  `cloud`, `tri`, `flower`, `gem`, `drop`, `star`, `ghost`, `capsule`, `ears`,
  `gumdrop`, `heart`, `clover`, `boulder`) paired with a colour swatch. They
  render as satin-matte SVGs with layered shading, living eyes (blinking and
  cursor tracking on larger avatars), and motion states for idle, thinking,
  working, waiting for approval, and error. Ambient breathing and gaze motion
  stop automatically when your OS **reduced motion** preference is enabled.
- **A custom uploaded image or GIF**.

Avatars are purely visual metadata in the desktop UI and do not affect model
calls, permissions, or CLI/Telegram behaviour.

## Subagents

A bot can start a **subagent** (`spawn_subagent`) for a focused sub-task. A
subagent is the same bot: same persona, same permission rules and grants, and a
tool set that can only be narrower than the parent's. It can never be more
permissive than the bot that started it.

## Planned

The design documents describe things that are not part of the profile yet:
runtime selection (which computer a bot controls), memory settings and per-bot
connector accounts.

## Sharing a bot: profile files and the template gallery

A bot's **shareable profile** is one JSON file (`*.praxi-bot.json`,
`"format": "praxi-bot-profile"`, `"format_version": 1`). It carries the name,
the instructions (persona), the model preference, the tool selection
(`allow` / `exclude`), the permission level and per-category rules, the skills
the bot expects (name and version only) and, optionally, scheduled-task ideas.

**Never in the file:** API keys and other secrets, vault entries, "always
allow" grants, the egress waiver (`allow_egress_after_reads`), memory,
knowledge sources, chats, connected apps and accounts (`mcp:*` tools), trusted
or other folders, CLI engine settings, the computer target, paired devices and
the avatar. The file is built from an allow-list, not by deleting fields, and
an export is refused when the persona looks like it holds a secret.

- Export: the **Export profile** button in the bot's settings, or
  `bot profile export <BOT_ID> [--out FILE] [--schedules]`. Owner only.
- Import: **New bot → Import from file** or
  `bot profile import FILE [--level LEVEL] [--model provider/model] [--yes]`.
  Import first shows a **preview**: the level the file asked for and the level
  the bot will get, every category, whether it could run shell commands, use
  files, the desktop, a browser or scripts, the tools, the skills it expects,
  what was left out, and the full persona text. Nothing is created until you
  confirm.
- **A ceiling, not a request.** You choose the highest permission level the
  new bot may get (default `read-only`). The file's level and every category or
  per-initiator rule are lowered to it, never raised. Unknown tools, app
  bindings and command-line engines are dropped with a note; an emptied tool
  list never turns into "every tool". File access starts at "no folders" until
  you allow the whole computer or choose folders. Skills are only listed
  (nothing is installed or trusted by an import), and scheduled-task ideas are
  shown but never created.
- Strictness: files over 64 KiB, unknown fields, a newer `format_version`,
  control or bidirectional-override characters, path-like skill names and
  secret-looking text are refused. Importing never reads a path or fetches a
  URL named in the file.
- **Template gallery.** The new-bot dialog offers built-in templates
  (Researcher, Inbox triage, Daily briefing, Coder, Family helper) with English,
  Turkish and German names. They are data files in
  `crates/daemon-api/templates/` in the same format; a test checks that every
  template parses and imports under a ceiling. They go through the same preview.

Export and import are written to the audit log (`profile.export`,
`profile.import`). Paired devices cannot reach these routes.


### Computer target

A profile's `[runtime]` section accepts `computer = "auto"`, `"pc"`, `"host"`,
`"browser"`, `"off"`, `node:<canonical peer public key>`, or
`server:<server key>` (default: `auto`). `local` is a legacy alias for `pc`.
`node:<canonical peer public key>` selects a specific paired desktop-capable
node; display names and differently cased keys are never used for lookup.

- **`server:<key>` (browser only)**: keeps the bot, its model providers and
  keys, file tools, shell commands, and approval prompts on **this** computer,
  and routes only `browser_*` tools to the paired always-on server identified
  by `<key>` (letters, digits, `-`, `_`, `.`, up to 128 characters; see
  `bot host devices` or your paired host list). Each bot gets its own isolated
  browser process and profile on the server (up to 8 open at once). Desktop
  tools (`desktop_*`) do not reach the server, and `browser_eval` (page
  scripts) is disabled there. It fails closed: if `<key>` is unknown, unpaired,
  or offline, the run has no computer tools and never falls back to this
  computer's browser or another server. The desktop Edit bot screen shows
  `"Paired server (browser only)"` read-only; set `server:<key>` in the TOML
  profile or via the API. See
  [Using a paired server as a bot's browser computer](remote-host.md#using-a-paired-server-as-a-bots-browser-computer).

`pc` selects a connected peer with the `desktop` role, or the local desktop on a
standalone PC. Server nodes are available only by explicit key. `auto` prefers a
PC, then the host desktop, then a browser. An explicit `host` requires a local
desktop; a missing display fails the run with a reason. Explicit `pc` fails if
peer networking is disabled and no local PC desktop is available.

Remote PC runs expose desktop tools only. Browser tools and browser workflow
steps are disabled because the browser driver runs on the daemon computer.
Choose `browser` for a browser on the daemon, or `host` for its desktop and
browser. Local PC runs can use their local browser. Headed and headless browser
sessions have separate session keys and profile directories, so they do not
share cookies or logins, and a run that switches launch mode does not reuse the
managed profile of the other mode.

The local computer id stored on grants and runs is this daemon's identity-key
fingerprint. The display name stays "Local computer". The word `local` is not
an identity: a grant or run written as `local` on one machine does not become
valid on another machine that receives `bot.db`.

Every run records `computer_target`, `computer_node`, and `computer_node_name`.
A resumed run uses its original PC key, including after a profile edit. A paused
run from before computer targets were stored has no target; resuming it binds
this daemon's own computer and does not follow `auto` onto a paired PC. If a
stored PC is offline (or the provider has not been installed yet), the run pauses
with `waiting_for_pc`; another PC or the host desktop cannot take over. A legacy
PC run without a recoverable node key also pauses rather than guessing. Runs that
have never selected a PC can bind their first PC when it arrives. Waiting expires
after 24 hours measured from `paused_at_ms`, independent of other row updates.
When `auto` has a paired PC that is not reachable and falls through to the host
or the browser, the run records that choice.

Computer approvals include the target kind, name, and key. Desktop/browser
"always allow" and "this chat" grants apply only to that concrete target
(including `server:<key>`). Old unscoped computer grants stay visible and
authorize nothing. API bot updates that omit `runtime` preserve its stored
value; conflicting runtime declarations in the request and profile are
rejected.
