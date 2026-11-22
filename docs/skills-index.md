# Skill index format (version 1)

A **skill index** is a JSON document, served over `https://` or kept as a local
file, that lists skills other people can install with
`bot skills install <index-url> --name <skill>`. The machine-readable schema is
[`skills-index.schema.json`](skills-index.schema.json); `bot skills index
validate` is the reference check.

```json
{
  "version": 1,
  "name": "Example skills",
  "skills": [
    {
      "name": "github-triage",
      "description": "Triage GitHub issues: labels, duplicates, templates.",
      "git": "https://github.com/example/praxi-skills.git",
      "commit": "0123456789abcdef0123456789abcdef01234567",
      "subdir": "github-triage",
      "sha256": "<fingerprint printed by `bot skills index validate --check-content`>",
      "tags": ["github", "triage"],
      "author": "Example Org",
      "license": "MIT",
      "homepage": "https://example.com/skills",
      "min_praxi_version": "0.4.0"
    }
  ]
}
```

## Fields

Top level: `version` (required, integer `1`), `skills` (required array),
`name`, `description`, `homepage` (optional, display only).

Per entry:

| Field | Required | Rule |
| --- | --- | --- |
| `name` | yes | The skill name: 1-64 characters of `a-z 0-9 - _ .`, starts with a letter or digit, no `..`. Must equal the `name` in the skill's `SKILL.md`. Unique within the index. |
| `description` | recommended | At most 200 characters. Display only. |
| `git` | yes | `https://` clone URL (no credentials, no whitespace). |
| `commit` | yes | **Full** 40- or 64-character hexadecimal commit hash. Branches, tags and short hashes are rejected, so an entry always means exactly one tree. |
| `subdir` | no | Relative folder of the skill inside the repository, no `..`. |
| `sha256` | no, strongly recommended | Content fingerprint (as printed by the install report). When present the installer refuses content that differs. |
| `tags` | no | Up to 10 tags of 1-32 characters `a-z 0-9 -`. |
| `author`, `license` | no | Display strings, at most 100 / 64 characters. |
| `homepage` | no | `https://` URL, at most 200 characters. |
| `min_praxi_version` | no | `MAJOR.MINOR.PATCH`. Informational today. |

Limits: the document is at most **1 MiB** and lists at most **1000** skills.

## Compatibility rules

- Readers **must ignore fields they do not know** (the validator only warns),
  so new optional fields can be added to version 1 without breaking anyone.
- A document with a `version` greater than the reader knows is rejected, never
  half-read. Changing the meaning of an existing field or making an optional
  field required needs a new version.
- Everything in version 1 is compatible with the entry shape the installer has
  always read (`name`, `description`, `git`, `commit`, `subdir`, `sha256`).

## Validating

```text
bot skills index validate ./index.json
bot skills index validate https://example.com/index.json
bot skills index validate ./index.json --check-content
```

The first two check structure only (schema, https-only URLs, full commit
hashes, duplicate names, size limits) and print every problem, not just the
first. `--check-content` additionally fetches each entry at its pinned commit
(it never installs anything), validates the skill, and compares `sha256`; for an
entry without one it prints the fingerprint to paste in. The exit code is
non-zero when anything is invalid, so the command fits a CI job.

## What an index does and does not prove

An index is a list of pointers. Installing from one still goes through the
normal preview: the static scan report is shown and the install is bound to its
fingerprint. The scan is advisory: no findings never means a skill is safe, and
an index author's `sha256` only proves the content did not change since the
author looked at it. Indexes are not signed; trust the publisher, not the
format. To withdraw a skill, remove its entry (and tell users to
`bot skills uninstall` it); already-installed copies stay until uninstalled.
