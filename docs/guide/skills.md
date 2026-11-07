# Skills

A **skill** is a short Markdown playbook that teaches a bot how to do something
tricky, for example "how to operate a custom dropdown" or "how to navigate
Windows Settings". Skills are not code: they are instructions the bot reads when
they are relevant.

The bot always sees a compact **index** of available skills (name and one-line
description). It loads a skill's full text only when needed, using the tools
`skill_list`, `skill_read` and `skill_hint`. A bot can also **write** a skill
(`skill_write`) to remember something it learned; those land in your skills folder.

## Where skills live

Later layers override earlier ones by name:

1. **Bundled** with Praxi (the repository's `skills/` folder).
2. **Yours**: `<data dir>/skills/`.
3. **Per bot**: `<data dir>/bots/<bot-id>/skills/` (supported by the skills
   library; the daemon currently loads the `skills/` folder in your data
   directory and saves what bots learn there).

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
- `version`: free text.
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
