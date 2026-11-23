# ADR: where do my bots run? (This computer vs Server)

Status: accepted. PR: #552. Code: `apps/desktop/ui/src/lib/runLocation.ts`.

## Context

The desktop app can talk to its own daemon or to an always-on server the owner
added. The old pill mixed "which host am I connected to" with "where do runs
happen", and Stop could show a run as live after its `run_finished` event was
lost. Users are often not programmers: they ask "is my bot running on my
laptop or on the server, and what happens if I close the lid?".

## Decision

- Two first-class places, shown as radio rows in the top-right popover:
  **This computer** (no server needed) and **Server <name>** (always on).
  Capability rows (view screen, terminal, sync, computer access) show only for
  the server. "Stop everything running on <place>" is a separate danger zone
  with a confirm and a live count.
- No new backend model. The daemon still models the server as the adopted
  "home" host; "This computer" is the app's `workLocalNow` override. The choice
  is stored client-side (`praxi.runLocation`, per browser profile) so it is a
  real default after a restart.
- Switching while runs are live asks first and stops nothing: a run keeps going
  where it started.
- Stop reports honestly ("Stopped 1 run on <place>" / "Nothing was running").
  `reconcile` closes runs the daemon does not list as live, so a lost
  `run_finished` cannot leave a stuck step in the panel.

## Consequences

- No migration and no new daemon route; old clients and hosts keep working.
- The preference is per app profile, not per bot and not synced. A second
  device starts with its own default.
- Stop uses the daemon's run list as the source of truth, never the UI's.

## Alternatives rejected

- A per-bot default location stored by the daemon: needs a schema change, and
  the per-bot target already exists server-side; deferred.
- Keeping one pill that mixes connection and run location: the wording could
  not be made plain.
