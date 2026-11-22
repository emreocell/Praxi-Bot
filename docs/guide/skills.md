# Skills

A **skill** is a short Markdown playbook that teaches a bot how to do something
tricky, for example "how to operate a custom dropdown" or "how to navigate
Windows Settings". Skills are not code: they are instructions the bot reads when
they are relevant.

The bot always sees a compact **index** of available skills (name and one-line
description). It loads a skill's full text only when needed, using the tools
`skill_list`, `skill_read` and `skill_hint`. A bot can also **write** a skill
(`skill_write`) to remember something it learned; those are saved in that bot's own folder,
`<data dir>/bots/<bot-id>/skills/`.

## Where skills live

Later layers override earlier ones by name:

1. **Bundled** with Praxi (the repository's `skills/` folder).
2. **Yours**: `<data dir>/skills/`.
3. **Per bot**: `<data dir>/bots/<bot-id>/skills/`. `skill_write` saves what a
   bot learns here, in the calling bot's own folder. The shared
   `<data dir>/skills/` folder is the published/shared layer.

Bundled skills today: `cookie-consent-banners`, `custom-dropdowns`,
`date-pickers`, `github-basics`, `shadow-dom-and-iframes`,
`windows-file-upload-dialog`, `windows-settings-navigation`.

## Write one

Create `<data dir>/skills/<name>/SKILL.md`:

```markdown
---
name: custom-dropdowns
description: Operate non-native dropdowns, comboboxes and autocomplete lists built from divs.
version: 0.1.0
when:
  keywords: [dropdown, select, combobox, autocomplete]
---
# Custom dropdowns

1. Click the control with `browser_click` to open it.
2. Run `browser_snapshot` again; options often appear at the end of the page.
3. Click the option by its ref and verify the control shows the chosen text.
```

Rules:

- `name`: 1 to 64 characters of `a-z`, `0-9`, `-`, `_`, `.`; must start with a
  letter or digit; no `..`; it is also the folder name and the override key.
- `description`: one line, at most 200 characters. This is what the bot uses to
  decide whether to read the skill, so make it specific.
- `version`: 1 to 64 characters of letters, digits, `.`, `-`, `_` and `+`; it
  must start with a letter or digit, must not contain `..` and must not end
  with `.`.
- `when` (optional) makes the skill surface automatically. All three keys are
  lists: `domains` (such as `github.com`; subdomains match), `apps` (application
  names such as `SystemSettings`, case-insensitive, `.exe` ignored) and
  `keywords` (matched against the task text).
- The whole file must be at most 64 KB. Extra files can sit in the folder next
  to `SKILL.md`.
- The index shown to the model has a size budget, so a few focused skills beat a
  hundred vague ones.

Invalid skill files are skipped with an issue noted, not fatal.

Skills also feed the router: the keyword and embedding search of the decision
ladder (K2) can suggest a skill for a request (see [Workflows](workflows.md)).

## Install a skill

Skills written by other people are untrusted content. `bot skills install`
never runs anything from a skill; it copies files into `<data dir>/skills/`
after you have seen what is in them.

```text
bot skills install https://github.com/someone/praxi-skills.git --subdir github-triage
```

1. The skill is fetched into a temporary folder, validated and scanned. You see
   the name, version, source, **pinned commit**, file count, and every finding
   of the static scan (scripts, network use, credential patterns, prompt
   injection phrases, obfuscated payloads), plus a warning when the folder holds
   script or binary files.
2. You confirm in the terminal, or pass `--accept <fingerprint>` (the
   fingerprint printed in the report). The fingerprint covers every file path
   and byte, so content that changed since you looked is refused.
3. The files are copied into your data folder and recorded in
   `<data dir>/skills.lock.json` (source, commit, content hash, time).

Rules that always apply: git and index sources always need the fingerprint,
and the CLI asks for it for local folders too; archives with symlinks, special
files, paths outside the folder or too many/too large files are refused; index
entries must pin a full commit hash. The scan is advisory: **no findings never
means safe**, read the files.

```text
bot skills install https://example.com/index.json --name github-triage   # from an index
bot skills install ./my-skill                                            # local folder
bot skills list                  # source, commit, risk, usage, MODIFIED/MISSING flags
bot skills update github-triage  # shows new findings and risk change, asks first
bot skills update                # check every hub-installed skill
bot skills uninstall github-triage
bot skills sync                  # restore skills whose folder is missing (verifies hashes)
```

An update re-fetches the recorded source and prints a diff of the scan result
against the installed copy (new and gone findings, risk before/after, old and
new commit). Replacing needs the new fingerprint, so approving the old version
does not approve the new one. Uninstall removes the folder, the lock entry and
the usage counters. `sync` refuses to restore a skill whose content no longer
matches the locked hash.

A running daemon picks up installed, updated and removed skills the next time
it reloads its skills; restart it if a change does not show up. Installed
skills land in the shared layer, so a bot can read them like any other skill.

## Managing shared skills through the API

The local owner (not a paired device) can create, import, update and delete
shared skills through the daemon API. See
[Managing shared skills from the local app](skill-management-api.md).

## Choosing which skills a bot gets

On a bot's profile, **Skills → Choose skills…** picks which shared and bundled
skills the bot is offered. The default is all of them. A bot's own learned
skills always stay active, and a skill the bot publishes is not assigned to
other bots automatically.

An unassigned skill is not listed in the bot's prompt, in its skill hint, in
the router's candidate list or in `skill_list`, and `skill_read` refuses it. If
the assignment cannot be read (a damaged row, an unusable bot folder), the bot
gets no shared skill rather than all of them.

This is a prompt and tool boundary, **not confidentiality**. Shared skills are
plain files in `<data folder>/skills/`; a bot that has been granted file access
to that folder or a shell can still read an unassigned skill from disk. Do not
keep secrets in a skill file.
