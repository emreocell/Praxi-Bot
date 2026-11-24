# Starter issues

Well-scoped pieces of work that a newcomer or an experienced contributor can pick up. Each one
is (or will be) a GitHub issue; comment on the issue to claim it before you start, so two people
do not do the same work. Read [CONTRIBUTING.md](../../CONTRIBUTING.md) first.

- **Size:** S = a few hours, M = a day or two, L = several days.
- **First-timer:** safe to start with if this is your first contribution.
- Areas under heavy change right now (the right panel, approvals, the Apps modal, host/server
  flows) only have issues here that stay valid after the open pull requests land; where one depends
  on a pull request it says "blocked until #N lands".

Each section below records its GitHub issue as `Issue: #N`. The tracking issue is #192 ("Good first issues and where to start").

## ISSUE: Add a third UI language
Issue: #163
labels: good first issue, help wanted, area: i18n, area: desktop-ui, enhancement
size: M · first-timer: yes (a translation, plus small registrations)

**Context.** The desktop UI ships English and Turkish (`apps/desktop/ui/src/i18n/locales/en.json`,
`tr.json`, about 2,600 keys). The steps for a new language are in
[docs/contributing/adding-a-language.md](adding-a-language.md). Pick a language nobody has claimed
(comment first) and add it.

**Done when**
- `i18n/locales/<code>.json` has every key of `en.json`, with placeholders (`{{name}}`) intact.
- The language is registered in `i18n/index.ts` and offered in the first-run wizard
  (`views/OnboardingView.tsx`) and in Settings > General (`views/settings/GeneralCards.tsx`).
- `pnpm --dir apps/desktop/ui check:i18n`, `typecheck` and `test` pass.
- The PR has screenshots (one dark theme, the light theme) in mock mode (`?mock=1&lang=<code>`).

**Pointers.** `apps/desktop/ui/scripts/check-i18n.mjs`, `lib/messageTime.ts` (clock formats).
One language per pull request.

## ISSUE: Make `check:i18n` compare every locale file
Issue: #164
labels: good first issue, area: i18n, area: desktop-ui, enhancement
size: S · first-timer: yes

**Context.** `apps/desktop/ui/scripts/check-i18n.mjs` hard-codes `en.json` and `tr.json`. With a
third language it would silently not check the new file.

**Done when**
- The script discovers every `*.json` in `src/i18n/locales/`, treats `en.json` as the reference and
  reports, per locale, keys that are missing or extra.
- Unknown `t("...")` keys are still reported once, against `en.json`.
- The existing output format (`i18n ok (N keys)`) is kept for the passing case.

**Pointers.** The script is about 40 lines; CI runs it as "UI i18n keys" (`.github/workflows/ci.yml`).

## ISSUE: Complete the keyboard shortcuts help
Issue: #165
labels: good first issue, area: desktop-ui, accessibility, enhancement
size: S · first-timer: yes

**Context.** The shortcuts dialog (`apps/desktop/ui/src/components/HelpModal.tsx`) lists only a few
shortcuts (command palette, send, stop, message recall) while the app has more: open Settings
(`Ctrl ,`), newline in the composer, closing dialogs with `Esc`, moving through the popup menus with
the arrow keys, and so on.

**Done when**
- Every global and composer shortcut that exists in the code is listed, grouped (Global, Chat
  composer, Dialogs), in English and Turkish (`help.*` keys).
- A short unit test or a check script fails when a shortcut is added to the code without a help
  entry **or** you document in the PR how the list is kept in sync.

**Pointers.** `grep -rn "onKeyDown" apps/desktop/ui/src`, `views/ChatView.tsx` (`onKeyDown`),
`App.tsx` (global handlers), `lib/` shortcut helpers.

## ISSUE: Refresh statements in the user guide that no longer match the app
Issue: #166
labels: good first issue, documentation
size: S · first-timer: yes

**Context.** Parts of `docs/guide/` were written before some features landed. The MCP, release,
Linux desktop backend and README statements have been refreshed (MCP servers are wired into the
daemon, `v0.1.0-alpha.1` is published, `crates/desktop-linux` has an X11 and AT-SPI backend). Still
to verify against the code and the running app: `faq.md` says that the
node-agent is not connected (its memory answer was refreshed), and the Linux node path has not been tried end to end. Verify each
statement and fix or remove it.

**Done when**
- Every statement you change is backed by a file path or a command you ran (cite it in the PR).
- Features that really are missing stay marked **Planned**.
- Relative links still resolve.

**Pointers.** `docs/guide/faq.md`, `index.md`, `remote-host.md`, `mcp-servers.md`; the MCP routes in
`crates/daemon-api/src/mcp_routes.rs`.

## ISSUE: Document connecting apps (the Apps dialog) in the user guide
Issue: #167
labels: documentation, area: apps, help wanted
size: M · first-timer: yes · **blocked until #160 lands**

**Context.** There is no user-guide page for the Apps dialog: direct OAuth apps (for example Linear,
Notion, Stripe, Atlassian), Composio apps, MCP servers under "Advanced", and what "Connected" means.
The flow is being reworked in #160, so write this page after it merges.

**Done when**
- `docs/guide/apps.md` explains connecting, signing out, per-bot access, and troubleshooting a
  connection that is stuck, with screenshots from mock mode.
- It is linked from `docs/guide/index.md`.

## ISSUE: Add a README to each core crate
Issue: #168
labels: good first issue, documentation, help wanted
size: S per crate · first-timer: yes

**Context.** The crates have a one-line `description` in `Cargo.toml` but most have no README. A
newcomer cannot tell where to look. CONTRIBUTING.md has the crate table; each crate deserves a
paragraph.

**Done when** (pick one crate per pull request: `permission-engine`, `scheduler`, `storage`,
`secret-store`, `composio`, `cli-provider`, `mcp-bridge`, `agent-router`, `tool-registry`, `protocol`)
- `crates/<name>/README.md` says what the crate is for, its main types/entry points (verified in the
  code), how to run its tests (`cargo test -p <name>`), and any invariant a change must keep.
- No claims you did not verify.

## ISSUE: Contribute example skills
Issue: #169
labels: good first issue, documentation, enhancement
size: S per skill · first-timer: yes

**Context.** Skills are markdown files (`SKILL.md`) that teach a bot how to handle a tricky site or
app. The repository ships seven in `skills/` (cookie banners, custom dropdowns, date pickers, GitHub
basics, shadow DOM and iframes, the Windows file-upload dialog, Windows Settings navigation). The
format is documented in `docs/guide/skills.md`.

**Done when**
- A new `skills/<name>/SKILL.md` solves a concrete, repeatable problem (for example a common
  sign-in form pattern, file pickers on Linux, a spreadsheet web app), follows the documented format
  and contains no secrets or personal data.
- You tested it with a bot and describe how in the PR.

## ISSUE: Add unit tests for the UI helper modules that have none
Issue: #170
labels: good first issue, area: desktop-ui, help wanted
size: S per module · first-timer: yes

**Context.** `apps/desktop/ui/src/lib/` holds the UI logic that can be tested without a browser
(`node --test`). These modules have no `*.test.ts` file yet: `avatar`, `backdropCatalog`,
`backdropDraw`, `botPatch`, `externalLink`, `focusTrap`, `inertStack`, `liveViewRelease`, `newChat`,
`ownerKey`, `popoverLayers`, `providerConnection`, `searchText`, `shell`, `shellContract`,
`shellLegacy`, `shellMutations`, `snapshotOrder`.

**Done when**
- A `lib/<name>.test.ts` covers the module's public behaviour with a few focused cases (follow
  `lib/appTiles.test.ts` for the style).
- `pnpm --dir apps/desktop/ui test` passes. Do not change the module's behaviour in the same PR; if you
  find a bug, open an issue.

## ISSUE: Keyboard-only and screen-reader pass over the first-run wizard, chat and settings
Issue: #171
labels: help wanted, accessibility, area: desktop-ui, enhancement
size: M · first-timer: no

**Context.** The UI already uses ARIA attributes widely, and there are focus-trap helpers
(`lib/focusTrap.ts`, `lib/inertStack.ts`), but there is no systematic accessibility check.

**Done when**
- You walk the first-run wizard, sending a message, approving a card, opening Settings and the Apps
  dialog with the keyboard only (and, ideally, with a screen reader), and list what fails.
- You fix the problems that are local (missing labels, focus order, focus return after closing a
  dialog, visible focus ring in every theme) and open separate issues for larger ones.
- `check:themes` still passes (focus rings keep their contrast).

**Pointers.** `components/Modal.tsx`, `views/OnboardingView.tsx`, `views/ChatView.tsx`,
`packages/ui/tokens.css` (`--shadow-focus`).

## ISSUE: macOS desktop backend: screenshots
Issue: #172
labels: help wanted, platform: macos, area: daemon, enhancement
size: M · first-timer: no

**Context.** `crates/desktop-macos` is a stub (a doc comment only) while Windows has a full backend
and Linux has X11 + AT-SPI. Bots cannot see the screen on macOS. This issue is the first, smallest
step: screenshot capture of the main display and of a window.

**Done when**
- `crates/desktop-macos` implements the screenshot part of the `Desktop` trait from
  `crates/desktop-api`, using the system capture API, with the required permission (Screen Recording)
  detected and reported as a clear error instead of a black image.
- It compiles on macOS and the crate still compiles (as an empty module) on other systems.
- Notes on how you tested it (macOS version).

**Pointers.** `crates/desktop-windows` and `crates/desktop-linux` for the shape; `crates/vision`
consumes screenshots. `unsafe_code` is denied workspace-wide: use a safe binding crate or isolate and
justify any `unsafe` (discuss in the issue first).

## ISSUE: macOS desktop backend: accessibility tree
Issue: #173
labels: help wanted, platform: macos, area: daemon, enhancement
size: L · first-timer: no

**Context.** To act on apps a bot needs the accessibility tree (roles, names, bounds, actions).
Windows uses UI Automation, Linux AT-SPI. macOS needs the Accessibility (AX) API.

**Done when**
- The `Desktop` trait's inspection methods work for the frontmost app and for a named app, including
  the Accessibility permission check with a clear error.
- A small fixture-based test of the tree-to-DTO conversion that runs without a display.

**Blocked until** the macOS screenshots issue is done (shared setup), or coordinate in the issue.

## ISSUE: macOS desktop backend: keyboard and mouse input
Issue: #174
labels: help wanted, platform: macos, area: daemon, enhancement
size: M · first-timer: no

**Context.** Synthesised keyboard and mouse input (the stub's doc comment mentions `CGEvent`).

**Done when**
- Click, double-click, drag, scroll, key press and typed text (including non-ASCII) work through the
  `Desktop` trait on macOS, honouring the Accessibility permission.
- Behaviour matches the other backends for the same trait calls (see their tests).

## ISSUE: Build and publish Linux and macOS desktop bundles
Issue: #175
labels: help wanted, platform: linux, platform: macos, area: desktop-ui, enhancement
size: L · first-timer: no

**Context.** `apps/desktop/tauri.conf.json` has `"targets": ["nsis"]`: only the Windows installer
exists. The daemon and the UI run on Linux/macOS from source, and Linux server packages are built by
`.github/workflows/linux-release.yml`, but there is no desktop bundle (AppImage/`.deb`, `.dmg`).

**Done when**
- Tauri bundle targets for Linux (AppImage and/or `.deb`) and macOS (`.dmg`) are configured, the
  sidecar `bot` binary is staged per target triple (see "Producing the Bundled Sidecar" in the README
  and `.github/workflows/installer.yml`), and a CI job builds them without publishing.
- Documented in `docs/guide/install.md`, with what is signed and what is not.

**Notes.** Signing and notarisation need the maintainers' accounts: the job must work unsigned and
say so.

## ISSUE: Wayland support for the Linux desktop backend
Issue: #176
labels: help wanted, platform: linux, area: daemon, needs design
size: L · first-timer: no

**Context.** `crates/desktop-linux/src/backend.rs` rejects Wayland sessions ("Wayland is out of
scope for v0"); it uses X11/XTest and AT-SPI. Many desktops now default to Wayland.

**Done when** (this issue is a design + first implementation step)
- A short design note in the PR/issue: which APIs (xdg-desktop-portal screen cast and remote desktop,
  libei, AT-SPI over D-Bus) cover screenshots, input and the accessibility tree, and what needs a
  permission prompt.
- A first working slice (for example screenshot via the portal) behind a runtime check, with the
  current X11 path unchanged.

## ISSUE: Windows on ARM64: build and installer in CI
Issue: #177
labels: help wanted, platform: windows, enhancement
size: M · first-timer: no

**Context.** The static-CRT build flags already cover `aarch64-pc-windows-msvc`
(`.cargo/config.toml`), but CI and the installer workflow only build x86-64.

**Done when**
- CI builds (and, if a runner is available, tests) the `cli` and `desktop` crates for
  `aarch64-pc-windows-msvc`, or documents precisely what blocks it (a dependency, a runner).
- The NSIS installer workflow can produce an ARM64 installer, or the issue lists the missing pieces.

## ISSUE: Winget manifest for the Windows installer
Issue: #178
labels: help wanted, platform: windows, enhancement, needs maintainer decision
size: M · first-timer: no · **blocked until a stable, signed release exists**

**Context.** Windows users expect `winget install`. Manifests need a stable download URL, a
SHA-256 and, ideally, a signed installer. The NSIS installer is currently unsigned (SmartScreen
warns).

**Done when**
- A manifest set (version, installer, locale) is prepared and validated with `winget validate`, with
  a short guide for the maintainers on how a release updates it (ideally automated).

## ISSUE: Homebrew cask or formula
Issue: #179
labels: help wanted, platform: macos, enhancement, needs maintainer decision
size: M · first-timer: no · **blocked until macOS bundles exist**

**Context.** Depends on the "Linux and macOS desktop bundles" issue (a `.dmg` for a cask) or, for the
daemon alone, a formula building `bot` from source (`cargo build --release -p cli`).

**Done when**
- A formula (and/or cask) installs and `bot --version` runs; `brew audit` passes; the release
  process to bump it is documented.

## ISSUE: AUR package for the Linux daemon
Issue: #180
labels: help wanted, platform: linux, enhancement
size: S · first-timer: no

**Context.** `scripts/install-linux.sh` and `docs/guide/remote-host.md` install the Linux host with
a verified release bundle. Arch users would like a `PKGBUILD`.

**Done when**
- A `PKGBUILD` (in `packaging/arch/`) builds `bot` from a tagged release (checksums pinned) and
  installs the binary and a systemd unit consistent with the one that `scripts/install-linux.sh` sets up.
- The README of that folder says how to test it in a clean container.

## ISSUE: Docker image for the always-on host
Issue: #181
labels: help wanted, area: host, platform: linux, enhancement
size: M · first-timer: no

**Context.** The always-on host is set up on a VM with the installer script
(`docs/guide/remote-host.md`). Some people would rather run `docker run` on a NAS or a container
host. There is no Dockerfile.

**Done when**
- `packaging/docker/Dockerfile` (multi-stage, non-root user, data directory as a volume) builds the
  daemon in host mode, with documentation for pairing it from the desktop app (see the invite step in
  `docs/guide/remote-host.md`) and for persisting the data volume.
- A CI job builds the image (no push) or the PR explains why not.
- Nothing is baked into the image that is secret.

## ISSUE: Run the real-app smoke harness in CI
Issue: #182
labels: help wanted, area: desktop-ui, area: daemon, enhancement
size: L · first-timer: no

**Context.** `pnpm --dir apps/desktop/ui smoke` drives the real UI against a real local daemon with a
scripted provider stub (`apps/desktop/ui/scripts/smoke.mjs`, steps in `smoke.spec.mjs`). It is local
only and can be flaky on slow machines, so regressions in real wiring are caught late.

**Done when**
- A Linux CI job installs a pinned Chrome (as the "browser integration" job does), builds the debug
  daemon, and runs the harness with retries only where a step is known to need them.
- Failing steps upload their screenshots as an artifact.
- Flaky steps are identified and fixed or quarantined with an issue link in the spec.

## ISSUE: Add more direct app connectors to the MCP catalog
Issue: #183
labels: good first issue, help wanted, area: apps, enhancement
size: M · first-timer: yes (with care)

**Context.** `crates/mcp-bridge/catalog.json` lists the MCP servers users can add with one click
(23 entries today: filesystem, GitHub, Linear, Notion remote, Stripe, Atlassian, Tavily, ...). Each
entry has an id, name, description, category, homepage, icon and an `install` recipe. Remote
servers that sign in with OAuth need no secret from the user.

**Done when**
- One or more new entries for well-known, maintained MCP servers (remote OAuth ones preferred),
  each verified by actually connecting and listing tools; the PR says how.
- A brand icon is available in the UI (see `apps/desktop/ui/src/lib/brandIcons.ts`; icons come from
  `simple-icons`) or the entry falls back gracefully.
- The catalog schema test passes (`cargo test -p mcp-bridge`).

**Pointers.** `crates/mcp-bridge/src`, the existing remote entries, `docs/guide/mcp-servers.md`.
Do not add servers that need broad local permissions without a clear justification.

## ISSUE: Add an LLM provider preset
Issue: #184
labels: good first issue, help wanted, area: daemon, enhancement
size: S · first-timer: yes

**Context.** Provider presets are small TOML files in `providers/catalog/` (Anthropic, Gemini,
Gemini-OpenAI-compat, DeepSeek, LM Studio, Ollama, OpenRouter today). Any service with an
OpenAI-compatible endpoint can be added.

**Done when**
- A new `providers/catalog/<id>.toml` follows the existing files (kind, base URL, auth, models
  endpoint, capabilities), cites the vendor's API documentation in a comment, and loads in the
  provider-catalog tests (`cargo test -p provider-catalog`).
- You tested one real chat completion (describe how; never paste a key).

## ISSUE: Sign in to Antigravity (`agy`) on a server from the desktop
Issue: #185
labels: help wanted, area: cli-engines, area: host, enhancement
size: M · first-timer: no

**Context.** On a server, Praxi can sign Claude Code and Codex in from the desktop app (paste-code
and device-code flows, `crates/cli-provider/src/login.rs`, routes in
`crates/daemon-api/src/cli_login.rs`). Antigravity has no `login` subcommand: `agy --help` lists none,
and sign-in happens in its first interactive run, so it falls back to "run this over SSH".

**Done when**
- You find out (and document in the PR) whether Antigravity offers any headless or device-code
  sign-in, or a documented way to hand it a credential.
- If it does: add a registry entry with matchers and tests like the existing fake-CLI tests; if it
  does not: improve the manual path (exact command, what to expect) and say so in the guide.
- Credentials are never copied from the desktop to the server.

## ISSUE: Sign in to Cursor's CLI on a server from the desktop
Issue: #186
labels: help wanted, area: cli-engines, area: host, enhancement
size: M · first-timer: no

**Context.** Same machinery as the Antigravity issue. `cursor-agent login` is listed as the manual
command (`crates/cli-provider/src/registry.rs`); its flow has not been driven from the app.

**Done when**
- The flow is investigated with a real install; if it prints a page and finishes by itself (or
  takes a code), a registry entry with strict URL host matching and tests with a fake CLI is added;
  otherwise the manual path is documented precisely.

## ISSUE: Sign in to OpenCode on a server from the desktop
Issue: #187
labels: help wanted, area: cli-engines, area: host, enhancement
size: M · first-timer: no

**Context.** `opencode auth login [target] [--method ...] [--answer ...]` is a provider/method form,
so it needs a choice before it can run. Today the app offers an API key as the alternative.

**Done when**
- A design for choosing the provider and method in the UI and passing them as fixed, validated
  arguments (never free text) is implemented for the providers whose flow works headlessly, with
  tests; other providers keep the API-key path.

## ISSUE: Natural-language schedule parser: more phrases and a third language
Issue: #188
labels: good first issue, help wanted, area: daemon, enhancement
size: M · first-timer: yes (Rust basics)

**Context.** `crates/scheduler/src/nl.rs` is a table-driven parser that turns phrases such as
"every weekday at 9" or `her gün 09:00` into a schedule spec (English and Turkish). It is pure and
deterministic, so it is easy to test.

**Done when**
- New phrase forms you found missing (list them with examples in the PR) are added with table tests,
  or a third language is added the same way (a separate table, not scattered `if`s).
- Existing tests still pass (`cargo test -p scheduler`); ambiguous phrases return `None` rather
  than guess.

## ISSUE: Add hermetic evaluation scenarios
Issue: #189
labels: help wanted, area: daemon, enhancement
size: M · first-timer: no

**Context.** `evals/scenarios/` holds YAML scenarios (17 today: approval ask/approve/deny, the
never-list, reads skipping the ask, ...) run by `crates/evals`. They check behaviour without a real
model.

**Done when**
- New scenarios cover behaviours that matter and are not covered yet (check the folder first), for
  example: egress after an untrusted read, a subagent's permission ceiling, protected paths for file
  tools, schedule-driven runs asking when they should.
- Each scenario states what it proves in a comment; `cargo test -p evals` passes.

## ISSUE: Stylelint/UI polish: audit components for raw values and missing tokens
Issue: #190
labels: good first issue, area: desktop-ui, help wanted
size: S · first-timer: yes

**Context.** Component CSS must use design tokens from `packages/ui/tokens.css` (stylelint forbids
raw `px`/`rem`/`em` and colour functions). Some older rules rely on `--*` fallbacks or repeated
values that should be tokens.

**Done when**
- You find (with `grep`) three to five places where a repeated literal or fallback value should be a
  token, add the token to `tokens.css` for every theme, replace the usages, and `check:themes` plus
  `pnpm lint` pass.
- Screenshots before/after in dark and light themes show no visual change.

## ISSUE: Improve error messages for the first-run provider test
Issue: #191
labels: good first issue, area: desktop-ui, enhancement
size: S · first-timer: yes

**Context.** During the first-run wizard a "Test connection" failure should tell the user what to do
(wrong key, wrong base URL, network, model name). Errors flow through the human-error layer
(`apps/desktop/ui/src/lib/humanError.ts`; texts in `errors.codes` of both locale files).

**Done when**
- You reproduce the common failures with mock mode or a local stub (wrong key, unreachable URL,
  unknown model), see what the user gets, and add clear title and hint texts (English and Turkish)
  for any that fall back to a generic message.
- A unit test in `lib/humanError.test.ts` covers the new cases.
