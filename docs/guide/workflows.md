# Workflows: record, replay, remember

Calling a language model for every click is slow and costs money. Praxi's answer
is to turn a task that worked once into something it can do again with **no
model calls**. There are three related mechanisms. Know which exist today:

| Mechanism | What it does | Status |
| --- | --- | --- |
| **Replay** | Run a saved, parameterised workflow deterministically | Available |
| **Remember** | Tell Praxi "this request should go straight to the route that just worked" | Available (desktop app button, API) |
| **Record** | Turn a successful run into a workflow file automatically | **Planned** in the product; the library exists but no CLI command or daemon route exposes it yet |

## Workflows

A workflow is a file `<data dir>/workflows/<name>.toml` (an `index.json` next to
it is rebuilt automatically). It has optional **parameters** and a list of
**steps**. Today you write these files by hand (or generate them with a script);
the bot finds them with `workflow_list` and runs them with `workflow_run`.

```toml
version = 1
name = "notepad_note"
description = "Open Notepad and type a note"

[[params]]
name = "text"
type = "string"          # string | number | bool
default = "hello"        # no default = required
description = "Text to type"

[[steps]]
kind = "launch"
executable = "notepad.exe"

[[steps]]
kind = "desktop_action"
app = "notepad.exe"
window_title_pattern = "*Notepad"   # * and ? wildcards, case-insensitive
action = "type"                     # click | type | set_value | key
value = "{text}"
[steps.target]
role = "edit"
name = "Text editor"
fingerprint = "9f2c0a41d3b7e855"    # recorded identity (hex)
```

Names use letters, digits and simple separators. `{param}` placeholders are
filled in when the workflow runs.

### Step kinds

| `kind` | Does |
| --- | --- |
| `launch` | Start a desktop application |
| `desktop_action` | Click, type, set a value or press a key chord on a located element in a window |
| `browser_action` | Click, fill, select, press a key or go back, on a located page element |
| `navigate` | Open a URL in the browser |
| `wait_for` | Wait (default 15 s) for a window title, page text, a CSS selector or network idle |
| `shell` | Run a command (through `shell_run`) |
| `tool` | Call any registered tool with a templated input |
| `agent` | An explicit "ask the model" sub-task |

Elements are found by several strategies in order of trust (role and name,
accessible label, CSS selector, position hints) and checked against a recorded
fingerprint so a renamed button is noticed.

### Running a workflow

- Tell the bot: "run the notepad_note workflow with text 'meeting at 3'". It
  calls `workflow_run` with `{"name": "notepad_note", "params": {"text": "..."}}`.
- Type the slash command: `/workflow notepad_note text="meeting at 3"`.
- Put it on a [schedule](scheduled-tasks.md) (through the API / desktop app).

Replay needs no model while the screen still matches the recording. The moment
it does not (an element is renamed or ambiguous, an action fails, a permission
is denied, an `agent` step comes up, a success check fails) replay **stops and
hands over** to the bot, reporting which step it reached, so the bot can carry on
from there. Every step still passes through the permission system, exactly like
a direct tool call; a workflow cannot do anything its bot may not do. The
`workflow_run` tool itself is in the `script` category.

## Remember

After a run **succeeds**, you can mark it as worth remembering. In the desktop
app press **Remember this** under the answer; through the API it is
`POST /v1/runs/{id}/remember`. From then on the same request text is routed
directly to the route that worked (a saved workflow with its parameters, or a
tool or skill hint) without asking the main model to decide.

What it can remember:

- the route the router chose for that request, or
- the saved workflow the model ran with `workflow_run`.

What it refuses: runs that did not succeed, subagent runs, runs that followed no
route and ran no workflow, and bots with the router turned off. If you edit or
delete a workflow, the routes remembered for it are dropped. Remembered routes
expire after a while.

## The decision ladder, briefly

Before the main model sees a message, Praxi tries cheaper levels in order, each
failing open to the main model:

| Level | Mechanism |
| --- | --- |
| K0 | Slash commands and your own regex rules |
| K1 | The remembered routes above |
| K2 | Keyword (BM25) plus, when a Gemini key is configured, embedding search over your workflows, skills and tools |
| K3 | A fast classifier model (`models.classifier`) that answers in strict JSON |
| K4 | The main model with the full loop |

A workflow is chosen automatically only at high confidence (0.9) and only when all
its required parameters are known; otherwise the result is a hint that narrows the
tools the model sees. In the CLI the decision is printed as `[route ...]`. Turn
the whole ladder off per bot with `[router] enabled = false`.

## Planned

- One-click **record** from a successful run (the recorder keeps only successful
  state-changing desktop, browser and tool calls, turns element references into
  stable locators and lifts literal values from your message into parameters;
  the result is a proposal you review before saving).
- A workflow editor in the desktop app.
