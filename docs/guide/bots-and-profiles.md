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
fallback   = ["ollama/llama3.2"]              # optional, informational for now

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

The `planner` and `fallback` fields are parsed and stored but are
informational in the current version: the loop runs on `executor`. The
`classifier` is used by the decision ladder when `[router]` allows it.

### Permissions in a profile

- Bots created by `bot setup` with the General preset get `mcp:*` and `email_*`
  in `[tools] allow`, so they can reach MCP servers and email. This only makes
  the tools visible: MCP tools and `email_send` still ask for approval under the
  default policy. Profiles saved earlier are not changed; to give such a bot the
  same reach, add `"mcp:*"` and `"email_*"` to its `[tools] allow` list. Which
  servers a bot may use is set on the server (`bots`), see the MCP servers guide.
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
| Replace a profile | `PUT /v1/bots/{id}` (API; desktop app) |
| Delete a bot | `DELETE /v1/bots/{id}`; only possible while it has no chats or runs |
| See or revoke its "always allow" grants | `bot grants list <bot-id>`, `bot grants revoke ...` |

The CLI has no command for editing or deleting bots yet; use the desktop app or
the HTTP API (see `docs/api/openapi.json`).

## Subagents

A bot can start a **subagent** (`spawn_subagent`) for a focused sub-task. A
subagent is the same bot: same persona, same permission rules and grants, and a
tool set that can only be narrower than the parent's. It can never be more
permissive than the bot that started it.

## Planned

The design documents describe things that are not part of the profile yet:
runtime selection (which computer a bot controls), memory settings, per-bot
connector accounts, and a profile import/export format for sharing bots.
