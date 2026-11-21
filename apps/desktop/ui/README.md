# Desktop UI

## Mock mode

The desktop UI can run without a daemon or Tauri for design reviews and screenshots:

```sh
pnpm --dir apps/desktop/ui dev
# Open http://localhost:1420/?mock=1
```

Mock mode is enabled in development by `?mock=1` (or at build time with `VITE_MOCK=1`).
The following query parameters are read by the mock UI in `src/App.tsx`:

| Parameter | Values / effect |
| --- | --- |
| `mock` | Set to `1` to enable mock mode in development |
| `lang` | `en`, `de` or `tr` (defaults to `tr` when omitted or unrecognized) |
| `screen` | Selects a mock review screen; supported routes include `settings`, `apps` and `onboarding` |
| `section` | Settings section to open when `screen=settings` |
| `panel` | Set to `1` to open the side panel |
| `control` | Run-bar state: `bot_driving`, `paused` or `human_driving` |
| `rail` | Positive numeric rail width; values below 150 select the mini rail |

Appearance parameters such as `theme` are handled by `appearanceFromQuery`; consult that helper for
the current supported appearance values. Mock-mode setup and parameter handling live in
`src/api/mockMode.ts` and `src/App.tsx`.

## Accessibility check

`pnpm --dir apps/desktop/ui check:a11y` runs axe-core over the mock screens listed in
`scripts/check-a11y.mjs` (add a screen there when you add one; it reads the same `?screen=` hooks as
above) and compares the result with `scripts/a11y-allowlist.mjs`. See CONTRIBUTING.md for the rules
of the allow-list.

## Real-app smoke run

`pnpm --dir apps/desktop/ui smoke` drives the REAL UI (vite dev, not mock mode) against a REAL
local `bot serve` that talks to a scripted OpenAI-compatible stub: 28 flows covering onboarding, bots, chat, tools and approvals, settings, apps, host/control behaviour, Stop handling, production origins and CLI setup, a screenshot per step,
exit code 1 on any failure. About 3 minutes after the first build.

- Needs Chrome (`CHROME_BIN` to override) and a Rust toolchain; builds `bot` into `CARGO_TARGET_DIR`
  (default `D:/praxi-target-shared`); `--no-build` reuses it. `--only=<text>` filters steps.
- Starts its own processes (stub, daemon, vite, Chrome) and stops them by PID; temp data dir is deleted.
- Tauri commands are shimmed for the browser (list printed at the end); screenshots go to `SMOKE_SHOTS`.
- Not in CI yet. To add: a job after the UI job running `cargo build -p cli` + `pnpm --dir apps/desktop/ui smoke`
  with `SMOKE_SHOTS` uploaded as an artifact (needs Chrome, preinstalled on GitHub runners).
- It cannot cover the Tauri shell, tray, real CLIs or a real host.
