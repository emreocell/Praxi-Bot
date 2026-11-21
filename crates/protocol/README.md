# protocol

The `protocol` crate provides the shared, Serde-serializable wire types used
between the Praxi-Bot daemon, agent runtime, storage, and clients. It defines
identifiers, event-stream payloads, initiators, and permission categories rather
than implementing transport or policy decisions.

## Entry points

- [`Envelope`](src/envelope.rs) wraps an [`Event`](src/event.rs) with schema
  version `v` (currently `1`), run-local sequence `seq`, emission time `ts_ms`
  in Unix milliseconds, and optional `parent_run_id` for subagent events.
  `Envelope::new` fills in the version and timestamp; `with_parent` records the
  parent run when applicable.
- [`Event`](src/event.rs) is the tagged client-stream event enum. It covers run
  lifecycle, message deltas, tool calls and approvals, todo updates, routing,
  workflow replay, desktop control, subagents, and errors. The individual
  event payload structs live in the same module.
- [IDs and `ChannelRef`](src/ids.rs) include `UserId`, `BotId`, `ChatId`,
  `RunId`, `NodeId`, `DeviceId`, `TaskId`, and `ToolCallId`. Each ID is a distinct
  UUID wrapper whose `new` constructor generates a UUIDv7. `ChannelRef::new`
  formats a channel reference as `<kind>:<address>`.
- [`Initiator` and `InitiatorKind`](src/initiator.rs) distinguish a human,
  routine, channel, subagent, or schedule that started a run. `Initiator`
  carries an identifier; `Initiator::kind()` returns its category.
  `InitiatorKind` also defines policy-only kinds, such as `ExternalAgent` and
  `ExternalSchedule`, without corresponding `Initiator` variants.
- [`PermissionCategory` and `Platform`](src/permission.rs) declare shared
  action categories and operating-system targets. Policy enforcement belongs
  to the permission engine, not this crate.
- [Approval text helpers](src/approval_text.rs), including
  `describe_details`, `scrub`, and `reveal_hidden`, support text-only approval
  surfaces and expose or remove hidden characters in attacker-influenced text.

## Where the types are used

- [`src/`](src/lib.rs) contains the Rust types and their Serde/JSON Schema
  implementations. For example, `agent-core` emits protocol events,
  [`storage`](../storage/src/event_log.rs) reads and writes `Envelope` values as
  per-run NDJSON, and [`daemon-api`](../daemon-api/src/sse.rs) streams them over
  `GET /v1/events`. The [Telegram adapter](../channel-telegram/src/bridge.rs)
  also consumes protocol events and approval text.
- [`schema/`](schema/Envelope.json) holds **committed JSON Schema snapshots**
  for selected public types, including `Envelope`, `Event`, identifiers,
  initiators, and permission types. The [snapshot tests](src/schema_snapshot.rs)
  generate schemas via `schemars` and compare them with these files; setting
  `UPDATE_SNAPSHOTS` regenerates them. They are not the daemon's OpenAPI
  generator or a runtime source of schema definitions.

## Compatibility invariants

- **Keep serialized event contracts deliberate.** `Event` uses a `type` tag
  with `snake_case` names, and `Initiator` uses `kind` and `id` fields. The
  `Envelope` version is currently `1`; its `seq` numbers events within one run.
  Optional fields added for backward compatibility need appropriate Serde
  defaults (for example `RouteDecided.tier`). Check existing clients and stored
  event logs before changing fields or tags.
- **Keep ID types distinct and review schema changes.** Rust ID newtypes
  prevent accidentally passing one kind of identifier as another. A change to
  a snapshotted wire type must be reviewed alongside the corresponding
  [`schema/` snapshots](schema/Event.json).
- **Check downstream API/client contracts separately.** The desktop UI
  [declares SSE types](../../apps/desktop/ui/src/api/types.ts), while the
  daemon's [OpenAPI description](../daemon-api/src/openapi.rs) is generated
  separately from route annotations and DTOs. Changes that affect those
  contracts may require checking the [OpenAPI snapshot](../../docs/api/openapi.json)
  and the generated UI API types; the protocol JSON Schema snapshots do not
  update OpenAPI automatically.

## Tests

From the workspace root:

```sh
cargo test -p protocol
```

This runs serialization/round-trip tests and checks the committed schemas.
For an intentional schema change, regenerate and review the diff, then rerun
without the update flag:

```sh
UPDATE_SNAPSHOTS=1 cargo test -p protocol
cargo test -p protocol
```

If a change also affects the daemon API contract, follow
[`CONTRIBUTING.md`](../../CONTRIBUTING.md) to check or regenerate the *separate*
OpenAPI snapshot (`UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot`)
and the UI API types when needed. No generated files should be changed for a
README-only contribution.
