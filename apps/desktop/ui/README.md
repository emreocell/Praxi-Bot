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

## Visual regression and wallpaper contrast

`pnpm --dir apps/desktop/ui check:visual` opens 8 mock screens (empty chat, chat with a run, settings, add-server
step 1, scheduled jobs, approval card, `/` and `@` popovers) in the Obsidian, Cobalt, Raven and Clay themes at
1280x800 and 640x800 (64 screenshots, English, still sky wallpaper, glass style) and checks two things:

- **Looks the same**: each screenshot is reduced to a 16x16 colour thumbnail and compared with
  `tests/visual/baselines.json` (thumbnail + perceptual hash per screen, about 70 KB, no PNGs in git).
  Thresholds are in `scripts/visual-lib.mjs` and are tolerant of font anti-aliasing and the clock in timestamps.
- **Readable on the wallpaper**: the text is hidden, the page is photographed again and every visible text run
  is measured against the pixels behind it (WCAG AA: 4.5:1, 3:1 for large text). Failures are listed (never
  allow-listed); the full list is in `report.json`.

`--only=<text>` filters screens, `--no-contrast` / `--no-compare` skip one half. `VISUAL_OUT=<dir>` keeps the
screenshots and the report. After an intended design change, rewrite the baselines and commit them:

```sh
pnpm --dir apps/desktop/ui check:visual:update      # all screens; add -- --only=chat-run for a few
```

In CI the job `Visual regression` is non-blocking (not part of `ci-result`) and uploads the screenshots.

## Real-app smoke run

`pnpm --dir apps/desktop/ui smoke` drives the production UI bundle against a real
local `bot serve` that talks to a scripted OpenAI-compatible stub: 28 flows covering onboarding, bots, chat, tools and approvals, settings, apps, host/control behaviour, Stop handling, production origins and CLI setup, a screenshot per step,
exit code 1 on any failure. About 3 minutes after the first build.

- Needs Chrome (`CHROME_BIN` to override) and a Rust toolchain. Builds `bot` and the fake coding CLI
  into `CARGO_TARGET_DIR` (default `target` at the repository root). `--no-build` reuses both binaries.
  `--only=<text>` filters steps. Include any steps that create state your selected step depends on.
- Headless Linux needs `xvfb`, `xauth`, `dbus-x11` and `at-spi2-core`. Run
  `xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp" dbus-run-session -- pnpm --dir apps/desktop/ui smoke`.
  The harness checks the local desktop backend and loopback provider before opening the UI,
  then stops at the first failed flow with its screenshot and logs.
- Starts and stops its own stub, daemon, Vite and Chrome processes. If Windows keeps an ACL-protected
  fixture file, the harness prints the temporary directory it retained.
- Tauri commands are shimmed for the browser (list printed at the end). Screenshots and logs go to
  `SMOKE_SHOTS`, which defaults to `praxi-smoke-shots` in the OS temporary directory.
- CI runs it on Windows and headless Linux for desktop and daemon changes. See the
  [maintainer guide](../../../docs/contributing/MAINTAINING.md#real-app-smoke) for failure artifacts.
- It cannot cover the Tauri shell, tray, real CLIs or a real host.
