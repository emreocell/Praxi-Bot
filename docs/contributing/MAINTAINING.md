# Maintaining: triage and merge guide

Goal: merging a good outside pull request should take minutes, not hours. This page is the
routine. It is written for a single maintainer; nothing here needs a team.

## The ten-minute routine

1. **Look at the labels.** A bot adds `area: ...`, `platform: ...` and `documentation` from the
   changed files. `area: security` means the change touches a guarded path: read it closely
   (step 4). No `area: security`, small diff, green CI: go to step 5.
2. **First-time contributor?** Open the Actions tab and press **Approve and run** on the
   waiting workflow *after skimming the diff* (see "Fork safety" below). Nothing runs until you do.
3. **Check the shape, not the style.** Is it one topic? Is there a test for new behaviour? Does
   the description say what and why? If not, leave one polite comment asking for exactly that and
   stop. Do not fix style by hand; ask the author or fix it after merge.
4. **Security-sensitive?** Read it against the invariant in
   [CONTRIBUTING.md](../../CONTRIBUTING.md#areas-that-need-extra-care): decisions stay in code,
   approvals and the egress gate are not weakened, secrets stay out of logs, owner-only routes stay
   classified. CODEOWNERS already requests your review for those paths. When unsure, ask for a
   smaller change or say "needs maintainer decision".
5. **Check the release notes labels** before merging. Apply `bug` for a fix, `enhancement` for a
   feature, or `tests` for test-only work. Check that the automatic `platform: ...` and
   `documentation` labels fit the change. GitHub uses the first matching category in
   [`.github/release.yml`](../../.github/release.yml), so a fix that also touches docs appears under
   Fixes. PRs without a matching label appear under Other changes.
6. **Wait for `ci-result` to be green**, then **squash-merge**. The squash title should be a
   conventional commit (`feat(scope): ...`, `fix(scope): ...`, `docs: ...`); edit it in the merge
   box if the author's is not.

## What decides "green"

Branch protection should require exactly one check: **`ci-result`**. It always runs and succeeds when
every needed job succeeded or was skipped on purpose, so a documentation-only (or UI-only) pull
request is not blocked waiting for jobs that never start. Inside it:

| Job | Runs when |
| --- | --- |
| `Docs links` | always (relative Markdown links resolve) |
| `Build & Test (windows-latest / ubuntu-latest)`, `Browser integration`, `License Check` | anything other than docs, repository metadata and `apps/desktop/ui/` sources changed (every push to `main` runs them all) |
| `UI Lint & Typecheck` (includes "generated API types are up to date") | UI files, shared UI packages, `docs/api/openapi.json` or `ci.yml` changed |

`docs/api/openapi.json` staleness is caught by the `openapi_snapshot` test inside Build & Test; the
failure message names the command to regenerate it. `schema.d.ts` staleness is caught by the UI job
with its own message. Neither needs your attention beyond telling the author to regenerate.

## Saying no kindly

- **Too big or mixed:** "Could you split this into the fix and the refactor? The fix alone I can
  merge today."
- **Needs design first:** apply `needs design` or `needs maintainer decision` and point to the
  [roadmap](../ROADMAP.md).
- **Out of scope** (telemetry, weakening a safety default, new dependency without an issue): say
  which line of [CONTRIBUTING.md](../../CONTRIBUTING.md#what-not-to-send) it falls under and close.
- **Stale** (no reply for four weeks): one reminder, then close with "happy to reopen".

## Fork safety (read once)

Rules the workflows follow, and that any new workflow must keep:

- `ci.yml` and `linux-scripts.yml` run on `pull_request`. For a fork, GitHub gives that event a
  read-only token and **no secrets**. These workflows never use secrets. Do not add any.
- `catalog-freshness.yml` runs on a weekly schedule (and `workflow_dispatch`) only on the main
  repository, with a read-only token, no network calls, and no secrets. It fails only on
  malformed catalog data or docs drift; staleness beyond 90 days is reported as a non-blocking
  warning in the job summary.
- `labeler.yml` is the only workflow using `pull_request_target` (it runs with write access to
  pull requests). It has **no checkout step** and only reads the list of changed files, so it never
  runs a fork's code. Never add a checkout, a build or a script step to it. If you must, switch to
  `pull_request` instead.
- `installer.yml` (signing secrets) is triggered only by tags, a weekly schedule and manual runs,
  never by pull requests; `linux-release.yml` publishes only from this repository, from a non-fork
  run, on a tag or `main`, and the final release also needs the reviewer-gated `release`
  environment. A fork cannot trigger either.
- Workflow files are CODEOWNERS-protected; review any change to `.github/workflows/` as code that
  runs with your permissions.

## Repository settings checklist (one-time, in the GitHub UI)

- **Settings > Actions > General > Fork pull request workflows from outside collaborators:**
  *Require approval for first-time contributors* (or stricter). Also keep the default workflow
  token **read-only**.
- **Settings > Code security > Private vulnerability reporting:** enabled (SECURITY.md points
  reporters to it).
- **Settings > Branches > branch protection for `main`:** require a pull request, require the
  status check **`ci-result`**, require branches to be up to date, and do not allow force pushes.
  Allow squash merging only.
- **Settings > Environments:** keep the `release` environment with a required reviewer.

## Keeping the model catalog fresh

Every ~90 days (or whenever `bot doctor` / the weekly `Model catalog freshness` workflow warns
that `last_reviewed` is stale), review the curated table in
`crates/provider-catalog/src/model_metadata.json` and the pricing/limits in
`providers/catalog/*.toml`:

1. **Inspect current status offline:**
   ```sh
   bot catalog check --docs docs/guide/model-catalog.md
   ```
2. **Diff against each provider's current listing:** save the provider's model-list JSON or TOML
   response to a local file (or pass `--live` with your own configured key) and run:
   ```sh
   bot catalog diff anthropic ./anthropic-models.json
   bot catalog diff gemini ./gemini-models.json
   ```
   Review added models, removed recommendations, changed limits/prices, and likely renames.
3. **Update the committed files:**
   - Edit `crates/provider-catalog/src/model_metadata.json` (and `providers/catalog/*.toml` if
     context limits or prices changed; keep `prices_as_of` updated).
   - Flag retired models with both `"deprecated": true` and a short `"note"` (such as `"retired"`
     or `"legacy"`), and remove them from recommendations.
   - Keep display names unique within each provider and never pin unstable `:free` ids as
     recommendations.
   - Update `"last_reviewed"` (top-level and any per-provider overrides) to today's `YYYY-MM-DD`.
4. **Sync the documentation table:** run `bot catalog render-docs` and replace the block between
   `<!-- BEGIN:model-catalog-table -->` and `<!-- END:model-catalog-table -->` in
   [docs/guide/model-catalog.md](../guide/model-catalog.md).
5. **Verify before opening the PR:**
   ```sh
   cargo test -p provider-catalog -p cli
   bot catalog check --docs docs/guide/model-catalog.md
   ```

## After merging

- Close the linked issue if GitHub did not (`Closes #123` does it).
- If the change was a good first issue, say thanks by name; the second contribution is the one that
  makes someone a regular.
