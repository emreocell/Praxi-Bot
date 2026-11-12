# Live view v1 and owned takeover (ADR 0029)

The common Rust contract is `crates/live-view`; the HTTP schema is in
`docs/api/openapi.json`. This checkpoint implements the daemon and control gates
against injected targets. Shipping requires the real adapters from 171/172,
the viewer from 173, the transport limits from 175, and the host measurements
from 174. The CLI currently exposes an empty registry until those existing
backend adapters are supplied. There is no screenshot-tool or second-browser
fallback.

## Authentication and capabilities

All `/v1/view/*` routes and `POST /v1/control/takeover` require the normal
loopback Host/Origin/bearer authentication or trusted paired `PeerIdentity`.
URL credentials are rejected. Local identity is the authenticated installation's
user ID; remote identity is the paired public key. `client_id` is correlation
only. Session UUIDs, takeover UUIDs, headers and request bodies do not grant an
identity. Desktop and phone roles can watch/input subject to separate
`screen:view` and `screen:input` privileges; node/host roles cannot use the API.
The local user has both by default; a paired device has neither until the local
user grants them per device with `PUT /v1/view/privileges/{peer_key}` (local only;
`input` requires `view`). Grants are held in memory: after a daemon restart a
paired device needs a new grant. Only the local UI or paired desktop may recover
an ownerless/expired pause with a view session.

`AppState::set_screen_privileges` changes policy as an input executor barrier
and invalidates the active owner's input; removing `view` also ends that actor's
sessions. `AppState::reconcile_paired_peers` applies the same to every
`peer:<key>` actor that is no longer paired; the CLI daemon runs it every two
seconds against the paired-device list. The transport must close revoked
peer bodies (175); the daemon also rechecks policy during streaming and before
every injected event. Neither SSE subscribers nor watching frames grant input.
Frame viewers do not register as overlay controllers.

## HTTP and wire contract

- `GET /v1/view/targets` lists registered opaque targets and capability reasons.
- `POST /v1/view/sessions` creates a watch session. Maximum two per actor, four
  per daemon, and two distinct capture targets. Session TTL is 30 seconds.
- `GET /v1/view/sessions/{id}/frames` requires
  `Accept: application/vnd.praxi.live-view.v1`; one stream per session.
- `PATCH /v1/view/sessions/{id}` updates size/quality/bandwidth and rendering
  feedback. Live profile requires active ownership.
- `PUT /v1/view/sessions/{id}/heartbeat` renews the session, and optionally a
  still-active input lease (15 seconds). An open stream never renews either.
- `DELETE /v1/view/sessions/{id}` cleans up and interrupts input, retaining pause.
- `POST /v1/control/takeover` durably closes global mutation admission and returns
  202/acquiring. Active is published only after admitted actions drain (10 second
  bound), input reset, pinning and audit. Retries use `request_id`.
- Input batches are authorised cheaply before they queue (input grant, own
  session, own takeover id) and fully re-validated under the executor. Up to 4
  batches per session and 16 in total may wait for it (FIFO, at most 1 s); beyond
  that, or after the wait, the answer is `429 input_busy`. A local Stop wakes all
  waiters so they give up at once, and the Stop's key reset and audit never queue
  behind them. Every backend call of a key reset is bounded (2 s); an unconfirmed
  reset leaves the Stop in force, answers 503 and is retried by the next `paused`
  heartbeat, also after a restart (the pending flags are in the latch file).
- `POST /v1/view/sessions/{id}/input` carries version, takeover/epoch, generation,
  geometry revision, first sequence number and typed events.
- `POST /v1/control/override` (local user only) interrupts any takeover, whoever
  owns it, resets pressed inputs and keeps the bot paused. It is idempotent (an
  app that re-sends `paused` every few seconds causes one latch write, one audit
  row, one event and one key reset) and terminal for paired devices: the
  overridden device loses its input grant, and neither it (`reacquire_takeover_id`)
  nor any other device (`recover: true`) can take the screen again while the
  latch carries the `local_override` reason. The reason is sticky inside the
  control hub: no later interruption (an expired or deleted peer session, a
  dropped stream, a privilege revoke, a failed key reset) replaces it, and the
  hub itself refuses a non-local takeover over it, so a fresh grant alone never
  suffices. Only the local user clears it
  (`recover`), and a new takeover then needs a fresh per-device grant. `PUT /v1/control/state`
  with `mode: paused` from the local user does the same while a takeover is owned.
- `POST /v1/control/recover` (local user only) clears a takeover latch, including
  one restored after a restart, without a view session or a registered target.
  A latch that never had a takeover owner is released with the ordinary release.
- A paired device that registers as the controller (`PUT /v1/control/state`)
  needs an `input` grant from the local user, like any other change of the latch
  or of the registered mode; a remote-host peer that does this must be granted
  first.
- A release that is open (journal written) when the local override lands is
  cancelled: a retry with the same `request_id`, or a new release, gets 403 and
  cannot clear the override or resume the bot. A failed key reset or audit row
  of the override is retried by the app's next `paused` heartbeat until it
  succeeds. The override survives a daemon restart (persisted as its own reason).
- Releasing or changing a latch the caller does not own (`POST /v1/control/release`
  without a body, `PUT /v1/control/state` with `release: true` or a different
  pause mode) needs the local user or a paired device that currently holds the
  `input` grant; without it the answer is 403. A latch left by a local override
  is local-only whatever the grants. Every `PUT /v1/control/state` from a paired
  device needs the `input` grant: without it a device can neither report a mode
  (the registration belongs to the local app) nor set a latch.
- `GET /v1/control/state` returns the takeover's owner (a short label,
  `peer:<first 8 characters>`, for paired devices other than the caller), target and state to
  everyone, but the takeover id, session id and epoch only to the owner (the
  others see nil ids and epoch 0).
- `POST /v1/control/release` requires owner, takeover/epoch and `request_id` while
  owned. `outcome` is `completed`; optional `human_action` is an enum, never free
  text. Bodyless release and `PUT state release:true` cannot bypass ownership.

Each binary record is `u32_be header_len | u32_be payload_len | JSON | payload`.
The header is at most 8 KiB; payload at most 2 MiB. Decoder lengths are checked
before allocation, and fragmented/concatenated HTTP chunks are supported.
Frames contain binary JPEG/WebP, never HTTP base64 or SSE pixel events. Status
and heartbeat have empty payloads. Responses set `Cache-Control: no-store` and
`X-Content-Type-Options: nosniff`; there is no disk buffering or replay.
Golden fixtures are in `crates/live-view/tests/fixtures`.

`FrameSubscription` holds a watch receiver: at most the latest pending immutable
frame, plus the HTTP record in flight. The body reserves bounded capacity before
encoding; a reader stalled for two seconds is disconnected. A publication with
old geometry/generation cannot authorize input. Source health older than two
seconds suspends input. Backends must bound raw allocations to 16 Mpixel and
stop capture on last unsubscribe. Watch does not launch or pin a browser.

Input batches are limited to 32 events/16 KiB. Coordinates are normalized to the
image content; positional input requires current geometry. Wheel axes are
bounded to +/-1200 CSS px; text is limited to 4096 UTF-8 bytes per event. Key codes
use the DOM allowlist, not raw OS keycodes. Per-actor limits are 120 events/s
with a 64-event burst and 8 KiB text/s. One executor orders input and release;
64 cached batch digests/results make retries idempotent without retaining text.
Gaps, mismatched retries and evicted retries require resynchronization. Partial
injection invalidates the epoch, resets keys best effort and retains pause.
No arbitrary CDP, JavaScript, clipboard, file-transfer or shell RPC is exposed.

## Continuation and lifecycle

The persistent global ControlHub latch applies to desktop, headed/headless
browser, tool calls from scripts, and workflow native mutations. Action permits
last through backend completion, including cancellation/partial failures. A script
that uses the computer is admitted by its first computer call, drains as one
bounded action and its later native calls inherit that admission (see below).
Independent shell/MCP processes are not suspended by this mechanism.

A paused acting call remains cancellable and does not consume its tool timeout
budget. Resume discards suspended tool arguments and obtains a normal fresh
observation; replay admits before resolving locators again. Observation epochs
also invalidate cached references in tools that were not waiting during takeover.
Computer runs share a run-lifetime slot (default one); children share the root's
slot. `BOT_MAX_COMPUTER_RUNS` overrides that limit. Managed Chromium closes after
120 seconds without real bot work (a run that used the computer and is still
active counts as work); `BOT_BROWSER_IDLE_SECONDS` overrides it.
The adapter must call `CdpDriver::set_takeover_pin` for active takeover only.

The global event stream (`GET /v1/events`, not replayed) carries
`control_state_changed` (`state` acquiring/active/paused/released/recovered,
`owner`, `target_id`, `reason`) for an indicator in a UI; it never carries ids,
epochs, pixels, text, keys or a full device key (`owner` is `local-ui:...` or the
short label `peer:<first 8 characters of the key>`). Release first announces
`paused` (input stopped), then `released`.

While the latch is `human_driving`, observation tools are refused as well
(browser snapshot/screenshot/tabs list/wait, desktop state/OCR/window list) with
an "observation paused" result. Plain `paused` (not human-driving) keeps today's
read behaviour: reads still work. The gate covers only the built-in browser and
desktop tools. It does NOT cover screenshot or read tools of MCP servers, nor
`shell_run` (a shell command can capture the screen or read files); those run
outside this gate, as do other independent processes.
A bot action that waits for the latch gives up after 30 minutes with a clear
message; the latch is not changed.

A script is admitted lazily: only its first computer (browser or desktop) call
takes admission, later computer calls of that script inherit it, and a script
that never touches the computer neither waits for a pause nor holds the computer
slot. Managed Chromium does not idle-close while a run that used the computer is
still active; its idle clock restarts when that run ends.

Denied attempts (`view.denied`, `input.batch_rejected`) are audited at most once
a minute per actor and event; the next row carries `suppressed_since_last` and `suppressed_by_reason` (a
bounded count per reason, so different denial reasons are not lost).

Audit volume: session open/close, takeover acquire/release/interrupt/denied and
input batches are audited; an open stream adds at most one `view.checkpoint` per
minute (heartbeat records still flow every 5 seconds). One capture source serves
all viewers of a target, so a viewer that asks for a very small size lowers the
size and quality all viewers of that target get; a per-viewer transcode is not
done on the small host.

Release stops input, resets injected keys, persists a payload-free journal/outbox,
adds one continuation message per affected chat (role `control_note`, fixed text
shape, delivered to the model as a system message) using durable message IDs,
audits release and clears the latch durably before waking calls. Notes contain
only event counts, target kind and optional allow-listed user report; login
success is never inferred. A finished run is not restarted. Retries and explicit
recovery reconcile delivered notes by message ID. Restart restores paused input;
uncertainty, corrupt files, audit/write/reset failures and expiry never resume
a bot. A recovery requires an explicit audited request and verified backend reset.
