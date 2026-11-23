# Agent built-in tools: audit and gaps

Status: gap 1 implemented in the `feat/c478-agent-tools` change; gaps 2-3 deferred (budget).

## What exists (beyond MCP)

| Family | Tools | Class |
| --- | --- | --- |
| Files | `fs_read/list/search`, `fs_write/edit/move/delete`, `checkpoint_*` | private read / local write / destructive |
| Documents | `pdf_extract`, `docx_*`, `xlsx_*`, `csv_*`, `json_query` | private read / local write |
| Web | `web_search`, `web_extract` (URL to Markdown, SSRF guard), `web_feed`, `web_sitemap`, `web_metadata` | untrusted + egress |
| Browser / desktop | `browser_*`, `desktop_*` | untrusted private + egress, some `executes` |
| Shell / code | `shell_run`, `script_run`, `git_*`, `process_list` | executes |
| Mail / calendar | `email_*`, calendar connector | private, untrusted, egress on send |
| Memory | `memory_search/save/forget`, `session_search`, `knowledge_search` | private read; writes ask when tainted |
| Scheduling | `schedule_*` (always ask for create/update/resume) | egress-like, delegated future authority |
| Bots, skills, workflows, secrets | `bot_*`, `skill_*`, `workflow_*`, `secret_request`, `secrets_list` | per tool |
| Meta | `ask_user`, `todo_write` | none, always allowed |

The two candidates named in the brief already exist: `web_extract` is the
SSRF-guarded fetch (every hop checked, size/time caps, untrusted + egress) and
`schedule_create` is the reminder / one-shot tool (always asks). They were not
re-implemented.

## Gaps, ranked

| # | Gap | Safety class | Decision |
| --- | --- | --- | --- |
| 1 | A scratchpad the bot can keep between runs (memory is user-confirmed facts only; there is no place for a plan, checklist or working data) | private; write is egress-like | **done**: `notes_read`, `notes_write` |
| 2 | The clock: models guess the date and time zone arithmetic | none (pure) | not done: see "Direct tool budget" |
| 3 | Exact arithmetic: models miscalculate sums, percentages, unit math | none (pure, no code execution) | not done: see "Direct tool budget" |
| 4 | Clipboard read/write | private (read), egress-like (write); needs an OS clipboard dependency per platform | not done: the clipboard is the most likely place for a pasted secret, and the cross-platform dependency needs its own review |
| 5 | Generic `http_request` (POST/PUT) | egress, destructive | not done: always-ask by construction, little value over `web_extract` + connectors |
| 6 | `download_file` to the workspace | untrusted write | not done: needs the file-origin mark and size policy of `fs_*` |

## Direct tool budget

The original change fit the last two slots of the 40-tool direct budget.
Notes use `notes_read` (no argument lists, `name` reads, `query` searches) and
`notes_write` (`mode` replace, append or delete). The installed catalog can now
grow independently of the request budget through [deferred tool loading](tool-discovery.md).

## Safety design of the new tools

* `notes_read`: `private_read` (a later egress after an untrusted read asks).
  Notes written in a run that held untrusted content carry an `untrusted`
  mark; reading them reports untrusted exposure and wraps the text in
  `<untrusted_note_data>`. An append from a clean run does not clear the mark.
* `notes_write`: declared `(untrusted=false, private=true, egress=true)` like
  `memory_save`, so a tainted run always asks before persisting or deleting;
  the approval card names "this bot's notes" and shows the full input. Refuses
  text that `secret_store::detect_secrets` flags, names outside
  `[A-Za-z0-9 _.-]`, and a missing bot id (fail closed). Limits: 64-char names,
  32 KiB per note, 100 notes, 512 KiB per bot.
* Storage: one JSON file per bot under `<data>/notes/`, named by a hash of the
  bot id; nothing from the model is ever a path.

## Permission levels

Notes use category `Fs` (as `memory_*`), so they follow the bot's file policy:
read-only presets allow the reads and deny the write; ask-before-write presets
ask for `notes_write` and `notes_delete` as for any write; `Full` still asks
on a tainted run (egress rule). Preset
`tool_allow` lists include `notes_*`.

Eval scenarios: `inj-notes-poison-after-page(-holdout)` (untrusted text cannot
write notes without approval) and `inj-notes-read-then-exfil(-holdout)`
(private notes + untrusted page cannot be sent out without approval).
