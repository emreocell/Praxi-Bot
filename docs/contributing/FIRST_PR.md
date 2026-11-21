# Your first pull request in 10 minutes

You do not need to understand the whole project. This path takes you from nothing to an open pull
request. Everything here also works for a small fix you found yourself; you do not need an issue
for a typo or a one-line fix.

## 1. Pick something small (2 minutes)

- Open an issue labelled [`good first issue`](https://github.com/emreocell/Praxi-Bot/labels/good%20first%20issue)
  and comment "I would like to take this" so nobody duplicates the work.
- Or fix a typo, an unclear sentence in `docs/`, or a missing translation. These need no setup at
  all: edit the file in GitHub's web editor and let it open the pull request for you.

Bigger idea? Open an issue first. For the chat screen, approvals and permissions, and host/server
flows, always discuss in an issue before writing code (see [CONTRIBUTING.md](../../CONTRIBUTING.md)).

## 2. Get the code (2 minutes)

```sh
git clone https://github.com/<your-user>/Praxi-Bot.git   # your fork
cd Praxi-Bot
git checkout -b docs/fix-typo-in-install-guide           # feat/..., fix/... or docs/...
```

Prerequisites are listed in [CONTRIBUTING.md](../../CONTRIBUTING.md#building-and-running-from-source).
Documentation and translation changes need none of them.

## 3. Make the change and check it (4 minutes)

Run only what matches what you touched:

| You changed | Run |
| --- | --- |
| Markdown in `docs/` or the root | `python3 scripts/check_markdown_links.py` |
| Rust in one crate | `cargo fmt --all -- --check` and `cargo test -p <crate>` |
| The desktop UI | `pnpm --dir apps/desktop/ui test` and `pnpm lint` |
| UI text | `pnpm --dir apps/desktop/ui check:i18n` |
| The HTTP API | `UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot`, then `pnpm --dir apps/desktop/ui gen:api`; commit both generated files |

Add one small test for behaviour you add or fix. Never put secrets, real hostnames or personal
paths in code, tests, docs or screenshots.

## 4. Open the pull request (2 minutes)

```sh
git add -A
git commit -m "docs: fix typo in the install guide"   # feat(scope): / fix(scope): / docs:
git push -u origin HEAD
```

Then click the link GitHub prints, and fill in the short template.

## What happens next

- **A maintainer approves the first CI run** for a first-time contributor. That is a safety
  measure for everyone, not a judgement of your change. CI starts right after.
- The only check that gates merging is **`ci-result`**. A docs-only or UI-only pull request
  skips the long Rust jobs, so it is green in a few minutes.
- A bot adds labels from the files you changed; you do not need to.
- A maintainer reviews, may ask for small changes, and **squash-merges** (your PR becomes one
  commit, so do not worry about a messy history).
- If a reply takes a few days, that is normal: this is a small team. A polite ping after a week is
  welcome.

Contributions are licensed Apache-2.0 like the rest of the project. There is no CLA to sign.
