# Skill scopes (task 139 / ARCH-02)

The daemon configures `SkillRoots::from_data_dir(data_dir)`. The root index is
read-only for learned skills. Every `skill_*` tool call resolves the supplied
`ToolContext.bot_id`; a missing id exposes only shared/bundled skills and rejects
`skill_write` and `skill_publish`.

| Scope | Location | Precedence | Writes |
| --- | --- | --- | --- |
| Bot | `<data_dir>/bots/<bot_id>/skills/` | First | `skill_write` for the calling bot |
| Shared | `<data_dir>/skills/` | Second | Explicit `skill_publish`; existing hub install flow |
| Bundled | Embedded shipped skills, plus optional `SkillRoots.bundled` | Last | Never |

`skill_list` and `skill_read` expose `scope: bot|shared|bundled`. The older
`source: user` label and `Source::User` Rust variant remain compatible for the
shared layer. Existing shared directories stay in place; there is no migration
or automatic publication of previously learned content.

## Safe writes and publication

- A bot id uses the workflow-name grammar: 1–64 lowercase ASCII letters, digits,
  underscores or hyphens. Windows device names are also refused on every host.
  Bot directories and skill entries cannot redirect a call through symlinks,
  junctions or other reparse points.
- `skill_write` without `expected_version` (or with `null`) is create-only in
  the bot scope, even if a shared or bundled skill has that name. To update a
  bot skill, pass the `expected_version` returned by `skill_read`. The comparison
  and write use one cross-process `fs4` lock. A stale version fails without
  changing the current file. A successful edit increments the final numeric
  version component (`0.1.0` → `0.1.1`); a nonnumeric final component gains `.1`.
  Versions must be safe path components because publication uses them in a
  directory name. Caller-supplied content cannot reset an existing version.
- A write uses a unique temporary file beside `SKILL.md`, syncs it, then uses
  `NamedTempFile::persist` to replace the file. It never deletes the current
  file to make a rename succeed. Failed replacement preserves current bytes.
- `skill_publish {"name":"x"}` copies the bot's complete skill tree into
  `<shared>/x@<version>/`. It is a registered `Effect::Write`, category `Fs`
  operation, so it follows the same permission/approval gate as other writes.
  A different effective shared skill requires `"replace": true`.
- Publication reuses the hub's hardened tree walker/copy helpers, including
  its reparse-point, size and Windows alternate-stream checks. The source
  and shared scopes are locked in that order. Copying is staged on the shared
  filesystem and checked against the source hash before publication. Directory
  swaps roll back if the final rename fails; a failed rollback preserves its
  backup and reports its location. Scope readers use the shared lock and see
  the complete previous or published version. Prior published versions of that
  name are replaced; original legacy/hub directories remain untouched.
- Published directories explicitly shadow legacy/hub directories of the same
  name. Hub `skills.lock.json`, source files, acceptance fingerprints, scanner
  decisions and provenance remain unchanged. A later hub install/update still
  requires its existing acceptance token. Publishing locally learned content
  is a separate, explicit write approval; it does not call or weaken hub install.

Locks are ordinary read/write handles, not append-only handles. Temporary files
and backup directories have unique names. Windows replacement does not assume
that a directory rename can overwrite a nonempty destination. Windows symlink
fixtures print a skip note if the runner lacks symlink privilege; the locked-file
replacement regression needs no symlink privilege.

## Remaining caller integration

`SkillIndex::prompt_block_for_bot(bot_id, context, suggested)` builds prompt
suggestions through exactly the same resolver as the tools. Its isolation is
covered by a regression test. The current `agent-core::exec::skills_block` has
no bot id in its `prompt_block_suggesting` call, and `AgentLoop` holds one index
for all bots. This PR leaves `agent-core` untouched as required by the batch's
file boundary. Until the owning branch changes that call, **automatic system
prompt suggestions include shared/bundled skills only**. Per-bot list/read/write
and publication are active through the daemon's `load_skills` wiring.

The follow-up is a single caller substitution using the run context's bot id:

```rust,ignore
let block = index.prompt_block_for_bot(
    Some(&ctx.bot_id.to_string()),
    &skills::SkillContext { domain: hints.domain, app: hints.app, text },
    suggested,
).ok()?;
```

Do not store a process-global “current bot” to work around this call site: runs
from different bots can execute concurrently.
