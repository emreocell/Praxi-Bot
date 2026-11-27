# Contributing to Praxi-Bot

Thank you for considering a contribution. Praxi-Bot is a self-hosted AI agent that runs on
your own machines: a local daemon, a desktop app, and an optional always-on Linux host. It is
**alpha software** (the latest pre-release is the only supported version), so the code moves
quickly and small, focused changes are the easiest to review and land. Pull requests of every
size are welcome: fixes, improvements and new features, from first-timers and regulars alike.

- **New to the codebase?** Read the [architecture tour](docs/contributing/architecture.md); all contributor docs are indexed in [docs/contributing/README.md](docs/contributing/README.md).
- **Never opened a PR here?** Follow [docs/contributing/FIRST_PR.md](docs/contributing/FIRST_PR.md): a 10-minute path from clone to pull request.

- **Looking for something to work on?** Start with the issues labelled
  [`good first issue`](https://github.com/emreocell/Praxi-Bot/labels/good%20first%20issue) and
  [`help wanted`](https://github.com/emreocell/Praxi-Bot/labels/help%20wanted), and with the
  [tracking issue "Good first issues and where to start"](https://github.com/emreocell/Praxi-Bot/issues/192) (the full text of each is in [docs/contributing/starter-issues.md](docs/contributing/starter-issues.md)).
- **Found a bug or have an idea?** Open an issue using one of the templates first when the change
  is more than a few lines, so we can agree on the direction before you spend time on it. For
  these areas, **discuss in an issue first, always** (they are where a good-looking change can
  quietly break a safety guarantee): the chat screen, approvals and permissions, and the
  host/server flows (pairing, host administration, moving bots between computers).
- **Found a vulnerability?** Do not open a public issue. See [SECURITY.md](SECURITY.md).
- **Be kind.** Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## The architecture on one screen

```
                  ┌────────────────────────────┐
  desktop app ───►│  bot  (daemon, 127.0.0.1)  │───► LLM providers (cloud API or local model)
  (Tauri + React) │  agent loop · tools ·      │───► coding CLIs (Claude Code, Codex, ...)
  CLI `bot ...`   │  permissions · scheduler   │───► MCP servers, Composio apps, e-mail, Telegram
                  └─────────────┬──────────────┘
                                │ paired, end-to-end encrypted (iroh)
                       always-on Linux host  (the same daemon, in host mode)
```

- **`bot`** (`apps/cli`) is the daemon and the command line. `bot serve` runs the local HTTP + SSE
  API (`crates/daemon-api`) in front of the agent loop. It only listens on `127.0.0.1`.
- **The desktop app** (`apps/desktop`) is a Tauri shell around a React UI (`apps/desktop/ui`). It
  starts the bundled daemon, talks to it over the local API and can also talk to a paired host.
- **The host** is the same daemon running on a Linux server (`bot host ...`, scripts in
  `scripts/`), so bots keep working when your computer is off.
- **`bot-node`** (`apps/node-agent`) is an experimental agent for controlled machines.

### Crates

| Crate | What it is for |
| --- | --- |
| `protocol` | Shared wire types (events, ids, permission categories) |
| `agent-core` | The agent loop, history building, approvals plumbing, router glue |
| `agent-router` | The "decision ladder": slash commands, remembered routes, workflow matching |
| `permission-engine` | Policy resolution, the never-list of catastrophic commands, the egress rules |
| `tool-registry` | Tool registration, discovery and search |
| `builtin-tools` | Built-in tools: files, shell, desktop, browser, scripts, meta tools |
| `browser-cdp` | Browser automation over the Chrome DevTools Protocol |
| `desktop-api`, `desktop-windows`, `desktop-linux`, `desktop-macos` | OS-agnostic desktop trait and the per-OS backends (macOS is a stub today) |
| `vision` | OCR and visual grounding helpers |
| `provider-catalog` | LLM provider adapters and the provider catalog |
| `cli-provider` | Detects and runs locally installed coding CLIs (Claude Code, Codex, Cursor, OpenCode, Antigravity, Qwen Code, GitHub Copilot CLI, Kiro CLI, Kimi Code CLI, Qoder CLI, Mistral Vibe, Goose), installs them and signs them in on a host |
| `pty-session` | A program on a pseudo-terminal (ConPTY / `openpty`): input, bounded text output, screen, resize, kill. Behind the `shell_interactive` tool and the CLI sign-in flows |
| `mcp-bridge` | MCP client and server bridge, OAuth for remote MCP servers |
| `assistant-import` | Finds the setups of other assistants (Claude, Cursor, VS Code, Codex, OpenClaw, Hermes, a folder) and plans what can be imported; behind `bot import` and the desktop Import screen ([design](docs/design/import-from-assistants-adr.md)) |
| `composio` | Composio app connections (REST client, curated catalog, logos) |
| `connector-email` | IMAP/SMTP tools |
| `web-tools`, `doc-tools`, `dev-tools`, `openapi-tools` | Web research, document/data, git/process and OpenAPI-import tools |
| `scheduler`, `task-runtime` | Durable schedules (cron, interval, one-shot, triggers) and background jobs |
| `missions` | Long-running missions: item ledger, per-session brief, supervisor that chains sessions, `mission_*` tools ([design](docs/design/missions-adr.md)) |
| `workflow-replay`, `skills`, `script-runtime` | Recorded workflows, markdown skills, the script sandbox |
| `memory-store` | Persistent memory and the knowledge/RAG store |
| `storage` | SQLite + NDJSON event log, migrations |
| `secret-store` | OS keychain / encrypted-file secrets and output redaction |
| `daemon-api` | The local HTTP + SSE API (axum), OpenAPI generation, the owner-route tables |
| `remote-api`, `peer-network`, `state-bundle`, `host-provision` | Pairing and the encrypted link to a host, state migration, server provisioning over SSH |
| `channel-api`, `channel-telegram` | Messaging transport contract, shared pairing/approval helpers, and the Telegram adapter ([writing an adapter](docs/contributing/writing-a-channel.md)) |
| `computer-target`, `live-view` | Per-bot computer selection and the live screen contracts |
| `plugin-host` | Reserved placeholder with no code yet; the plugin design is tracked in [issue #213](https://github.com/emreocell/Praxi-Bot/issues/213) |
| `evals` | The evaluation harness (`evals/`) |

The user-facing behaviour is described in the [user guide](docs/guide/index.md); the HTTP API is
documented in [`docs/api/openapi.json`](docs/api/openapi.json).

## Building and running from source

### Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.89 or newer (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` selects `stable` with `rustfmt` and `clippy`) | Edition 2024 |
| Node.js | 24 or newer | CI uses 24 |
| pnpm | 10 or newer (CI uses 12) | Workspace in `pnpm-workspace.yaml` |
| C toolchain | MSVC build tools on Windows; a normal C toolchain on Linux/macOS | |

- **Linux** also needs the desktop build dependencies (the same list CI installs):
  `sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libdbus-1-dev pkg-config libxcb1-dev`.
- **macOS:** the workspace is not built on macOS in CI and the macOS desktop backend
  (`crates/desktop-macos`) is a stub, so desktop control does not work there yet. The daemon and
  the UI are expected to build; reports and fixes are welcome (see the `platform: macos` label).
- **Windows** is the best-supported desktop platform today (UI Automation backend, NSIS installer).

Run `node scripts/check-prereqs.mjs` from the repository root to check your prerequisites before building.

### Build and run

```sh
pnpm install --frozen-lockfile        # UI dependencies (workspace root)
cargo build -p cli                    # the daemon + CLI: target/debug/bot
./target/debug/bot --version
./target/debug/bot setup              # interactive: provider, key, first bot, permission preset
./target/debug/bot serve              # the daemon on 127.0.0.1:47821 (leave it running)
```

In a second terminal: `bot status`, `bot bots list`, `bot chat <bot-id> "hello"`. The full command
list is the [CLI reference](docs/guide/cli-reference.md).

- **Data.** Everything lives in the data directory (`--data-dir` / `BOT_DATA_DIR`; otherwise the
  per-user app-data folder of your OS): a SQLite database, event logs, `providers.toml`, skills,
  workflows and the API token (`api-token`, readable only by your user). API keys go to the OS
  keychain or an encrypted file. Use a throwaway directory while developing:
  `bot --data-dir ./.dev-data serve`.
- **Desktop app in development** (Tauri; starts the daemon itself):
  `PRAXIBOT_FAKE_PROVIDER=1 pnpm dev:desktop` uses a deterministic fake provider, so no API key is
  needed. (`PRAXIBOT_BOT_BIN` points the app at a specific `bot` binary.)
- **UI only, no daemon, no Tauri:** `pnpm --dir apps/desktop/ui dev` and open
  `http://localhost:1420/?mock=1`. Mock mode (`apps/desktop/ui/src/api/mockMode.ts`) serves canned data
  and is how design reviews and screenshots are made. See the [Desktop UI README](apps/desktop/ui/README.md#mock-mode)
  for the current mock-mode query parameters.
- **Windows installer:** see [Building the Windows installer yourself](docs/guide/install.md#building-the-windows-installer-yourself).
- **Linux host:** see [Running Praxi on a remote host](docs/guide/remote-host.md).

## Testing

Run the tests for the crates you touched; the whole workspace takes a long time (about 40 minutes
on Windows in CI).

```sh
cargo test -p <crate>                       # e.g. cargo test -p scheduler
cargo clippy -p <crate> --all-targets -- -D warnings
pnpm --dir apps/desktop/ui test             # UI unit tests (node --test)
```

`doc_convert` / `doc_preview` (crate `doc-tools`) hand hostile documents to LibreOffice. The default
test run never needs `soffice`; before wiring them for untrusted documents, or after changing the
seeded LibreOffice profile or upgrading LibreOffice, run the real thing once by hand:

```sh
python scripts/check_soffice_no_network.py            # skips (exit 0) when soffice is missing
python scripts/check_soffice_no_network.py --require  # fails (exit 2) when soffice is missing
python scripts/check_soffice_no_network.py --self-test  # offline; also run by `cargo test -p doc-tools`
```

It converts linked-image, attached-template and `WEBSERVICE()` documents and fails if a local
listener on 127.0.0.1 receives a single request. CI may run it on a runner that has LibreOffice,
but that is optional.

CI runs, on **both Linux and Windows**: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and the workspace tests (`cargo nextest run --workspace`
plus `cargo test --workspace --doc`; plain `cargo test --workspace` runs both locally). It also runs
`cargo deny check licenses bans advisories sources`, the browser integration tests (Chrome for
Testing) and the UI checks below. It also checks that `docs/api/openapi.json` and
`apps/desktop/ui/src/api/schema.d.ts` are up to date: after an API change run
`UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot` and then
`pnpm --dir apps/desktop/ui gen:api`, and commit both files. Pull requests that only change
documentation, the marketing site (`site/`), packaging manifests (`packaging/`) or the UI sources
(`apps/desktop/ui/`, `packages/`) skip the long Rust jobs, but still run the Markdown link check
(`python3 scripts/check_markdown_links.py`) and the guide-index check
(`python3 scripts/check_guide_index.py`): run both locally when you edit `docs/` or a README. A PR
that only changes UI sources also skips the Windows real-app smoke run (Linux still runs it; every
push to `main` runs both). A PR is merged only when CI is green.

**How the pull request jobs are laid out.** The workspace tests run with
[cargo-nextest](https://nexte.st) (the `ci` profile in `.config/nextest.toml`, no retries) and the
doctests with `cargo test --doc`. The Windows workspace tests are split in two jobs that run at the
same time: `Build & Test (windows-latest)` runs formatting, Clippy and the first half of the tests;
`Build & Test (windows-latest, tests 2/2)` runs the second half and the doctests. nextest picks the
half by a hash of the test name, so a test stays in its half when others are added. `ci-result`
depends on both. Required checks: `ci-result` is the one gate that covers everything; the existing
job names are unchanged, `Build & Test (windows-latest, tests 2/2)` is new, and
`Real-app smoke (windows-latest)` does not appear on UI-only pull requests (`ci-result` accepts a
skipped job, a branch rule that lists the job names itself would wait for it forever).

**Evaluation scenarios.** Safety behaviour (approvals, the egress gate, prompt injection) is tested by
hermetic YAML scenarios in `evals/scenarios/`, run by `cargo test -p evals` with no network and no
real keys. When you change a gate or add a tool that reads untrusted content, add or update a
scenario: see [adding eval scenarios](docs/contributing/eval-scenarios.md).

To measure recent CI runs locally, use `python scripts/ci_report.py --runs 30`.
See [reading CI timing reports](docs/contributing/ci-report.md) for prerequisites,
queue/run-time definitions, JUnit artifacts and flaky-candidate limitations, and
the [November 2026 findings](docs/contributing/ci-report-2026-11.md) for the first sample.

**Test report (slow and flaky tests).** A scheduled workflow, `Test report`
(`.github/workflows/test-report.yml`), runs the workspace tests on Linux and Windows and the browser
integration tests with [cargo-nextest](https://nexte.st), retrying a failed test up to twice. Its
job summary lists the flaky tests (failed, then passed on a retry), the tests that failed on every
attempt, the 20 slowest tests and the 20 slowest test binaries. The same data is in the
`test-report-*` artifacts (`report.json` and the raw `junit.xml`). A red check on your pull
request that names a test from that flaky list is probably not your change; say so in the pull
request. The pull request checks themselves use nextest without retries. Doctests are
not part of the report (nextest does not run them). To measure locally:

```sh
cargo install cargo-nextest --locked                        # once
cargo nextest run -p <crate> --profile ci-report            # retries, writes JUnit XML
python3 scripts/test_report.py target/nextest/ci-report/junit.xml
```

Every Monday the `Test report (weekly)` workflow (`.github/workflows/test-report-weekly.yml`) reads
the nightly reports of the past seven days and posts one comment on the open issue titled "Weekly
test report" (it creates the issue on the first run). The comment lists the tests that were flaky or
failing on several nights, the 10 slowest tests and test binaries per OS, and the median, 90th
percentile and maximum duration of each pull request job. To build the same comment locally (needs
the `gh` CLI, logged in):

```sh
mkdir reports && gh run list --workflow test-report.yml --limit 7 --json databaseId --jq '.[].databaseId' |
  while read -r id; do gh run download "$id" --pattern 'test-report-*' --dir "reports/$id"; done
python3 scripts/ci_report.py --runs 40 --no-artifacts --json-out ci.json --output /dev/null
python3 scripts/test_report_weekly.py reports --ci-json ci.json
```

The CLI reference is checked too: `apps/cli/tests/cli_reference.rs` fails when a visible
command, long flag or environment variable is missing from `docs/guide/cli-reference.md`, or
when a heading names a command that no longer exists. Update the guide, or add a reasoned entry
to the test's allow-list, in the same pull request.

**Accessibility check.** `pnpm --dir apps/desktop/ui check:a11y` builds the mock bundle, opens about
40 screens and states (home, chat, right panel, approval card, schedules, apps, dialogs, first-run
wizard, every settings section; two themes, plus tr/de samples) in headless Chrome (`CHROME_BIN`) and
runs [axe-core](https://github.com/dequelabs/axe-core) on each. It takes about 40 seconds, runs in the
UI CI job and fails on any violation that is not in `apps/desktop/ui/scripts/a11y-allowlist.mjs`, and
also on an allow-list entry that no longer matches (delete it once you fix the problem). Prefer fixing
a violation (a name, a role, a heading level). If you cannot fix it now, add one entry
`{ rule, selector, screens, reason }` with a comment saying why; the list is visible debt, not a
parking lot. axe-core is MPL-2.0 and used unmodified as a dev dependency only; it is not shipped.
Use `--only=<substring>` (for example `--only=settings-ai`) to run a few cases while iterating.

**The real-app smoke run** drives the production UI bundle against a real local
daemon (a debug `bot serve`) that talks to a tiny scripted OpenAI-compatible stub, using headless
Chrome (set `CHROME_BIN` if it is not found):

```sh
pnpm --dir apps/desktop/ui smoke             # builds the daemon and CLI fixture, then runs the flows
pnpm --dir apps/desktop/ui smoke --no-build  # reuse both debug binaries
```

It covers: app start, creating bots, chatting, the model picker, tool calls and approval cards,
the bot panel, `@mention` answers, Settings, the Apps modal, the add-server wizard, the
human-takeover latch, the production origins and the owner-route checks. `SMOKE_SHOTS` selects an
output directory for screenshots and logs, and `CARGO_TARGET_DIR` selects the Rust build directory.
See the [maintainer guide](docs/contributing/MAINTAINING.md#real-app-smoke) for CI triggers and
failure artifacts.

## Conventions that are enforced

Run these before pushing; CI runs them too.

```sh
cargo fmt --all -- --check                  # formatting (rustfmt.toml)
cargo clippy --workspace --all-targets -- -D warnings   # pedantic lints are on workspace-wide
pnpm typecheck                              # TypeScript, all packages
pnpm lint                                   # eslint + stylelint
pnpm --dir apps/desktop/ui check:i18n       # translations
pnpm --dir apps/desktop/ui check:themes     # contrast of every theme
pnpm --dir apps/desktop/ui check:layout     # shell layout
cargo deny check                            # when you change dependencies
```

- **Rust.** `unsafe_code` is denied workspace-wide. Keep clippy pedantic clean; prefer fixing the
  code over adding `#[allow]`, and give any `#[allow]` a one-line reason.
- **i18n.** The UI has three locales: `en`, `tr` and `de`. Every user-facing string lives in all of
  `apps/desktop/ui/src/i18n/locales/{en,tr,de}.json`, and the files are kept sorted
  (`node apps/desktop/ui/scripts/sort-locales.mjs` does it). Write real translations, not copies of
  the English text: `check:i18n` fails when the key sets differ, a `t("...")` key is missing or the
  files are unsorted, and it warns about values identical to English. Keep placeholders such as
  `{{name}}` identical. Scaffold locales (`fr`, `pt`, `id`, listed in
  `apps/desktop/ui/src/i18n/scaffold-locales.json`) are English copies translated part by part: you
  do not have to add your new keys there (the app shows English for a missing key and `check:i18n`
  only warns), but extra keys, wrong placeholders and unsorted files still fail. Adding a language: see
  [docs/contributing/adding-a-language.md](docs/contributing/adding-a-language.md).
- **Themes and CSS.** Use design tokens (`packages/ui/tokens.css`); stylelint rejects raw `px`,
  `rem`, `em` units and colour functions in component CSS. `check:themes` verifies contrast for
  every theme.
- **Generated files are never edited by hand.** `docs/api/openapi.json` and
  `apps/desktop/ui/src/api/schema.d.ts` are generated:
  ```sh
  UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot   # rewrites docs/api/openapi.json
  pnpm --dir apps/desktop/ui gen:api                           # rewrites src/api/schema.d.ts
  ```
  Commit both together with the route/DTO change.
- **Errors reach the user through the human-error layer.** Daemon errors carry a stable `code`;
  `apps/desktop/ui/src/lib/humanError.ts` maps each code to a title and hint in every language.
  A test checks that every code the daemon can emit has text. New code → new entry in
  `DAEMON_ERROR_CODES` and an `errors` entry (title and hint) in all three locale files; run the
  whole `pnpm --dir apps/desktop/ui test`, not only your own test.
- **Owner-only routes are classified.** Routes that change authority, secrets, providers or keys,
  install or run software, pair devices, take over control or decide an approval are listed in
  `OWNER_ROUTES` (`crates/daemon-api/src/owner.rs`); ordinary mutating routes are in
  `TOKEN_OK_ROUTES`. A test fails when a documented mutating route is in neither table, so a new
  route cannot ship unclassified. When unsure, make it owner-only.
- **Migrations are numbered and forward-only.** SQL files in `crates/storage/migrations/` and
  `crates/scheduler/migrations/` are registered in order in `migrate.rs` / `store.rs`. Never edit a
  migration that has shipped; add a new numbered file, check the number is not already taken by an
  open PR, and keep the schema-version constant in step.
- **Commits** follow [Conventional Commits](https://www.conventionalcommits.org/):
  `feat(scheduler): ...`, `fix(daemon-api): ...`, `docs: ...`.

## Opening a pull request

1. **Fork** the repository and create a branch from an up-to-date `main`:
   `feat/<short-description>`, `fix/<short-description>` or `docs/<short-description>`.
2. **Keep it small and focused.** One topic per PR. A refactor and a feature are two PRs. If you
   plan something big, open an issue first and wait for a maintainer's answer.
3. **Add a test** for the behaviour you add or fix (one focused regression test is enough; see
   existing tests next to the code). UI logic goes in `apps/desktop/ui/src/lib/*.ts` with a
   `*.test.ts` file so it can be tested without a browser.
4. **Run the gates** listed above for what you touched.
5. **UI changes need screenshots** in the PR: in mock mode (`?mock=1`) at least one dark theme and
   the light theme, and all three languages (en, tr, de) if text changed.
6. **Fill in the pull request template.** Say what and why, how you tested it, which
   security-sensitive areas you touched, and whether generated files were regenerated.
7. **CI must be green.** The check that gates merging is `ci-result`; it runs on every PR and
   passes when the jobs that were needed passed (a docs-only or UI-only PR skips the long Rust jobs). A
   maintainer reviews, may ask for changes, and squash-merges. This is a small team, so there is
   no guaranteed response time. Answer review comments with a push or a reason rather than
   resolving them silently. Security-sensitive changes get a
   deeper review and may take longer. Your first PR from a fork needs a maintainer to approve the
   workflow run before CI starts; that is normal.
8. Link the issue (`Closes #123`). By opening the PR you agree to the licensing terms below.

Maintainers can follow the [triage-and-merge guide](docs/contributing/MAINTAINING.md) and the
[release runbook](docs/contributing/releasing.md).

## Areas that need extra care

A change touching these gets a maintainer review focused on the invariant below. Say so in your PR.

- **Approvals and permissions** (`permission-engine`, `agent-core/src/approval.rs`). Permissions are
  decided by code, never by the model. Every tool call - including calls made from scripts and
  workflows - passes the same gate; subagents are never more permissive than their parent; money
  and the never-list always ask, even at the most permissive level.
- **The egress gate** (`permission-engine/src/egress.rs`, `agent-core/src/taint.rs`). After a run has
  read untrusted or private content, anything that can send data off the machine asks, even with a
  standing grant. Do not add waivers; do not make a new tool "read-only" to avoid the check.
- **Secrets** (`secret-store`, redaction). Keys live in the keychain / encrypted file, never in
  config files, command lines, logs, audit rows or error messages. New code that touches a secret
  must keep it out of `Debug` output and zeroise buffers where practical.
- **Pairing and host administration** (`peer-network`, `remote-api`, `daemon-api/src/host_admin.rs`,
  `owner.rs`). The daemon trusts a peer only by its authenticated key and role; host administration
  needs an explicit, per-request check of the paired desktop's admin capability and the owner key
  for authority changes. Bots and plain API tokens must never reach these routes.
- **Protected paths** (`builtin-tools/src/files.rs`). File tools refuse the daemon's data
  directory, credential files and the CLIs' sign-in stores. Do not widen what file tools may write.
- **The shell tool and the never-list** (`builtin-tools/src/shell.rs`,
  `permission-engine/src/never_list.rs`). Commands are parsed, not pattern-matched on the raw
  string; the never-list is damping, not a sandbox - do not describe it as one.

## What not to send

- Drive-by reformatting or renames that bury a change; mass edits of generated files.
- New dependencies without an issue first (they must pass `cargo deny` and be justified).
- Anything that phones home, adds telemetry or analytics, or sends data to a service the user did not
  configure. The project's decision is **no telemetry**.
- Features that weaken a safety default (an approval, a protected path, a redaction) for convenience.
- Secrets, real hostnames/IP addresses, personal paths or account names in code, tests, docs or
  screenshots. Use documentation addresses (`203.0.113.0/24`) and `example.com`.

## Licence of contributions

Praxi-Bot is licensed under the **Apache License 2.0** (see [LICENSE](LICENSE) and [NOTICE](NOTICE)).
Contributions are accepted under the same licence: **inbound = outbound**. By submitting a
contribution you agree that it is licensed under Apache-2.0 (section 5 of the licence), and that
you have the right to submit it. There is **no CLA** to sign, and a `Signed-off-by` line
(DCO) is **not required**. Third-party material must be compatible with Apache-2.0 and listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
