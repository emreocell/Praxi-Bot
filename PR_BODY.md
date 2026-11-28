## What / why

Standing orders (scheduler #1031) could only be created through the API. This adds the owner-facing form in the schedule editor and surfaces daily run usage and last verdict on each row.

## Key decisions

- **Run now outside active hours**: owner **Run now** bypasses the active-hours gate (daily cap still enforced), consistent with treating it as an explicit owner action; scheduled firings still respect hours.
- **Bot-made standing orders**: stay read-only in the UI (`origin === "bot"`), same as before.
- **Last verdict**: derived from the last assistant message of succeeded runs and returned on history rows (`standing_verdict`); list DTO adds `standing_runs_today` / `standing_max_runs_per_day`.
- **Guide screenshot**: capture script could not reach `schedule-standing` in mock mode in this run; placeholder images copied from `new-schedule` until capture is fixed.

## How verified

```bash
export CARGO_TARGET_DIR=/srv/webioo/praxi-work/target/cur-l31b
cargo test -p scheduler
cargo fmt --all
cd apps/desktop/ui
pnpm typecheck
pnpm test
pnpm check:i18n
pnpm lint   # from repo root via pnpm lint
```

Results: scheduler 130+ tests OK; UI tests pass; i18n OK; eslint/stylelint OK.

**Not run here**: `check:a11y` (Chromium/Playwright `schedule-standing` screen added but not re-run in full), `docs:screenshots` for `schedule-standing` (timeout), Tauri desktop build (Linux).

## Not verified

- Full `check:a11y` shard over all screens
- End-to-end standing order against a live daemon
- Guide screenshot asset (placeholder only)