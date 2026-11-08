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
Only the local UI or paired desktop may recover an ownerless/expired pause.

`AppState::set_screen_privileges` changes policy as an input executor barrier
and invalidates the active owner's input. The transport must close revoked
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
- `POST /v1/view/sessions/{id}/input` carries version, takeover/epoch, generation,
  geometry revision, first sequence number and typed events.
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
last through backend completion, including cancellation/partial failures. An
already admitted script drains as one bounded action and its inner native calls
inherit that admission. New scripts wait before starting their runtime budget.
Independent shell/MCP processes are not suspended by this mechanism.

A paused acting call remains cancellable and does not consume its tool timeout
budget. Resume discards suspended tool arguments and obtains a normal fresh
observation; replay admits before resolving locators again. Observation epochs
also invalidate cached references in tools that were not waiting during takeover.
Computer runs share a run-lifetime slot (default one); children share the root's
slot. `BOT_MAX_COMPUTER_RUNS` overrides that limit. Managed Chromium closes after
120 seconds without real bot work; `BOT_BROWSER_IDLE_SECONDS` overrides it.
The adapter must call `CdpDriver::set_takeover_pin` for active takeover only.

Release stops input, resets injected keys, persists a payload-free journal/outbox,
adds one system continuation message per affected chat using durable message IDs,
audits release and clears the latch durably before waking calls. Notes contain
only event counts, target kind and optional allow-listed user report; login
success is never inferred. A finished run is not restarted. Retries and explicit
recovery reconcile delivered notes by message ID. Restart restores paused input;
uncertainty, corrupt files, audit/write/reset failures and expiry never resume
a bot. A recovery requires an explicit audited request and verified backend reset.
