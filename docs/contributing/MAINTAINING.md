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
every needed job succeeded or was skipped on purpose, so a documentation-only pull
request can skip jobs outside its scope.

| Job | Runs when |
| --- | --- |
| `Docs links` | always (relative Markdown links resolve) |
| `Build & Test (windows-latest / ubuntu-latest)`, `Browser integration`, `License Check` | anything other than docs, repository metadata and `apps/desktop/ui/` sources changed (every push to `main` runs them all) |
| `UI Lint & Typecheck` (includes "generated API types are up to date") | UI files, shared UI packages, `docs/api/openapi.json` or `ci.yml` changed |
| `Real-app smoke (windows-latest / ubuntu-latest)` | Desktop app, CLI daemon, shared crates or UI packages, provider or skill assets, relevant build inputs, or `ci.yml` changed. Also runs on pushes to `main`. |

`docs/api/openapi.json` staleness is caught by the `openapi_snapshot` test inside Build & Test; the
failure message names the command to regenerate it. `schema.d.ts` staleness is caught by the UI job
with its own message. Neither needs your attention beyond telling the author to regenerate.

## Real-app smoke

The two smoke jobs drive the desktop UI in pinned headless Chrome against a real debug `bot serve`
process. The harness uses fresh temporary state, dummy credentials, a scripted loopback provider
and search server, and a fake coding CLI. It shims the native Tauri commands. CI builds only the
daemon and CLI fixture, caches Rust and pnpm dependencies, and cancels superseded PR runs.
Linux runs inside Xvfb with a private D-Bus session: UI-created bots target this PC and need a
local desktop backend even for a text-only turn. The harness preserves the display authorization
and session bus in its isolated child environment, checks the backend and loopback provider at
startup, and stops at the first failed flow. Peer networking stays disabled; no paired PC is needed.

Each run has an eight-minute harness deadline. When a job fails, download `desktop-smoke-windows-latest`
or `desktop-smoke-ubuntu-latest` from the workflow run. The artifact contains any screenshots taken
before failure and the available build, harness, daemon and Vite logs. A failed smoke job fails
`ci-result`. A skipped smoke job satisfies it.

To reproduce locally, run `pnpm install --frozen-lockfile`, set `CHROME_BIN` if Chrome is outside
its usual install location, then run `pnpm --dir apps/desktop/ui smoke`. Set `SMOKE_SHOTS` to pick an
output directory. Pass `--no-build` to reuse a previously built debug daemon and CLI fixture, or
`--headed` to watch Chrome. `--only=<text>` can select a flow by name, but stateful flows require
the preceding setup steps. See the [desktop UI README](../../apps/desktop/ui/README.md#real-app-smoke-run).

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
- `installer.yml` (signing secrets) is triggered by tags, a weekly schedule and manual runs; a pull
  request runs it only when it touches the installer itself, and then without secrets, without the
  attestation job (`id-token: write`) and without the release-upload job (`contents: write`).
  `linux-release.yml` publishes only from this repository, from a non-fork run, on a tag or
  `main`, and the final release also needs the reviewer-gated `release` environment. A fork
  cannot trigger either. Every action in `installer.yml` is pinned to a commit SHA and every job
  lists its own `permissions`; `scripts/tests/test_windows_release.py` fails the build if not.
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

## MCP catalog verification

`scripts/verify_mcp_catalog.py` checks **every** entry of `crates/mcp-bridge/catalog.json`. It is
offline by default, so it can run on pull requests; `--live` adds the checks that need the network.

```sh
python3 scripts/verify_mcp_catalog.py                       # offline: what CI runs
python3 scripts/verify_mcp_catalog.py --live --timeout 5    # also registries, homepages, endpoints
python3 scripts/verify_mcp_catalog.py --only supabase --live --json /tmp/report.json
python3 scripts/verify_mcp_catalog.py --render-docs en      # the generated guide table (en | tr)
```

| Check | Offline | `--live` |
| --- | --- | --- |
| Valid shape: id, name, description, category, https homepage, https endpoint or a command, secrets declared, no token-looking literal | yes | |
| A stdio package is pinned to an exact version (`name@1.2.3`, `name==1.2.3`) | yes | the version exists on npm / PyPI |
| `license` is noted: an SPDX expression for software Praxi runs, or `hosted` for a vendor-run server | yes | differs from the registry's licence: attention |
| `auth` is declared (`none`, `oauth-dcr`, `oauth-own-client`, `api-key`) and agrees with `required_secrets` / `oauth_client` / the transport | yes | a remote endpoint contradicting it: attention |
| The logo licence is known: the `icon` is a Simple Icons slug bundled in `brandIcons.ts` (CC0), is recorded in `THIRD_PARTY_NOTICES.md`, or is `null` | yes | |
| `docs/guide/app-catalog.md` and its Turkish twin hold the table generated from the catalog | yes | |
| Homepage answers | | yes (a site that only turns bots away counts as answering) |
| Remote endpoint answers, OAuth metadata is found, dynamic client registration is advertised | | yes (`check_mcp_catalog.py`, below) |

An offline failure exits 2 and fails CI. A live finding is an *attention* row (dead homepage or
endpoint, missing package, no dynamic registration, licence or sign-in that changed): it never
fails a build, only `--strict` (by hand) exits 1. The weekly `MCP catalog check` workflow runs the
live mode and keeps one tracking issue up to date.

**Adding an entry.** Use only an official server, from the vendor or listed in the MCP registry.
Set `license` (`hosted` for a remote server), `auth`, a pinned package for a local one, and an
`icon` only when its licence is known (otherwise `null`; the app draws initials). Run the offline
check, paste `--render-docs en` / `--render-docs tr` between the table markers of
`docs/guide/app-catalog.md` and `docs/guide/tr/app-catalog.md`, add the id to `FIXTURES` and
`CANNOT_FAKE_YET` in `crates/mcp-bridge/tests/catalog_contract.rs`, and say in the pull request
what you connected to (and with which account) and what you could not.

## MCP catalog check (remote endpoints)

The HTTP entries in `crates/mcp-bridge/catalog.json` are verified by a credential-free script
(the live mode of `verify_mcp_catalog.py` calls it for every remote entry).
Run it by hand when editing the catalog, or read the weekly report from the
`MCP catalog check` workflow (Mondays 05:17 UTC, main repository only):

```sh
python3 scripts/check_mcp_catalog.py --timeout 5
python3 scripts/check_mcp_catalog.py --only linear --json /tmp/mcp-report.json
python3 scripts/check_mcp_catalog.py --strict   # also fails on attention lines; manual use only
```

For each HTTP entry it POSTs an unauthenticated MCP `initialize`, then follows
`WWW-Authenticate` / `/.well-known/oauth-protected-resource` /
`/.well-known/oauth-authorization-server` to report whether dynamic client registration is
advertised (`oauth-dcr`) or not (`oauth-no-dcr`). No authorization header or account is used, and
no response bodies are stored. Stdio entries are skipped here; they are covered by
`cargo test -p mcp-bridge --test catalog_contract`.

`--strict` makes attention lines (dead endpoints, no-DCR servers) exit 1 as well; use it by hand
only, never in a workflow that gates other work. The checker contacts only public `https` hosts:
it refuses IP literals, `localhost`, private or non-global addresses, other schemes, and every
redirect, including for the metadata URLs a vendor advertises. A vendor answer that cannot be
parsed or fetched becomes an attention row for that entry, never a failed run.

The text table is the human view; `--json` writes `checked_at`, `entries`, `attention` and
`structural_errors`. `attention` covers dead/changed endpoints and no-DCR servers: these are
reported in a single tracking issue (created, updated or closed by the scheduled workflow) and
never fail unrelated CI. Only a broken catalog schema (exit 2) fails the scheduled job. The
offline unit tests run in the normal `docs-check` CI job:

```sh
python3 -B -m unittest discover -s scripts/tests -p test_check_mcp_catalog.py -v
```

## After merging

- Close the linked issue if GitHub did not (`Closes #123` does it).
- If the change was a good first issue, say thanks by name; the second contribution is the one that
  makes someone a regular.
