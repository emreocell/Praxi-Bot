# Secure input request: a bot asks for a credential, the value goes straight to the vault

Status: slices 1 and 2 of issue #422 (this note and the daemon side). The desktop card (slice 3)
and the leak canaries and audit rows of #220 / #222 follow; the card contract below is what that
slice must implement.

Related: #218 (vault), #219, #220, #221, #222. Existing parts used here: `secrets_list` and
`browser_fill` with `secret` (`crates/builtin-tools/src/fill_secret.rs`), the fill store
(`crates/secret-store/src/fill.rs`), the owner key (`crates/daemon-api/src/owner.rs`), the taint
gate (`crates/agent-core/src/taint.rs`).

## Flow

1. The model calls `secret_request` with a `name`, a `purpose` and which `fields` it needs.
2. The tool validates the input (below) and returns a card description. It never receives a value.
3. The agent loop parks the run (`WaitingApproval`), records the request, writes an audit row
   (`secret_request.requested`) and emits a `secret_requested` event.
4. The owner's desktop app draws a card from that event. The owner fills it in and the app posts it
   to `POST /v1/secret-requests/{call_id}` with the owner key.
5. The daemon claims the request (once), checks the name rule, writes an audit row
   (`secret_request.submit`: name, fields, origins, programs, never a value), stores the value under
   `fill/<name>` with the allow-lists the owner confirmed (the store's value hook adds it to the
   redactor in the same call), and wakes the run.
6. The tool result the model sees is only `stored` or `cancelled` plus the name. It then uses
   `secrets_list` and `browser_fill` with `secret: "<name>"` as today, which still asks on every use
   (or follows a standing grant the owner made).

Cancel (`POST /v1/secret-requests/{call_id}/cancel`), the timeout and a stopped run all end with
`cancelled` and nothing stored.

## Tool

Name `secret_request`, category `meta`, effect `write` (so a policy that asks for writes asks first,
and a tainted run always asks). Offered only when the daemon has the fill store **and** requests are
switched on.

| Field | Type | Rule |
|-------|------|------|
| `name` | string | `fill/` entries only. The same rules as every fill name: letters, digits, `.` `_` `-` and `/`, at most 64 bytes, and `composio/`, `mcp/`, provider, e-mail and `__` names are refused. The `fill/` prefix is added by the store; the model never writes a different namespace. |
| `purpose` | string | Plain text, one line (control and bidirectional characters collapse to spaces), 1 to 200 characters. Refused, not trimmed, when longer. Refused when it contains `<` `>` backtick `[` `]` `*` `_` `\|`, `://`, `www.` or `mailto:`: no markup and no links. |
| `fields` | `secret` \| `login` | `secret` = one value, stored under `<name>`. `login` = username and password, stored under `<name>/username` and `<name>/password` (a `login` name is at most 50 bytes). Default `secret`. |
| `replace` | boolean | Default `false`. See "No read-back, no silent overwrite". |

Nothing else can be asked for: no card numbers, no free-form fields, no URL, no allow-list written by
the model. The allow-lists (origins, programs) are chosen by the owner on the card; the model may
suggest nothing, so a planted request cannot pre-fill "where this can be typed".

The tool's own output is a card description with no value. If it is ever called outside the agent
loop (a script, a bridge) nobody is asked and the text says so.

### Result the model sees

Only one of:

- `stored: the owner saved `<name>`. Use it by name ...`
- `cancelled: nothing was stored for `<name>` (<reason>).` (marked as an error result). The reason is
  one of: the owner cancelled, the owner did not answer in time, requests are switched off, this run
  cannot ask (not an owner desktop run), too many requests in this run.

No value, no username, no origins, no length.

## Event the card renders: `secret_requested`

Journaled like every run event (it holds no value), delivered on the local SSE stream and
**not** to paired peers (`sse::hidden_from_peer`).

```json
{
  "type": "secret_requested",
  "run_id": "...",
  "call_id": "...",           // the id to submit or cancel with
  "name": "login/example",    // entry name, without the fill/ prefix
  "purpose": "Sign in to ...", // untrusted model text, plain text, at most 200 chars
  "fields": "login",          // "secret" | "login"
  "replaces_existing": false, // the bot asked to replace an entry that exists
  "untrusted_content": true,  // the run already read untrusted content
  "expires_in_secs": 300
}
```

The card closes when the call completes (`tool_call_completed` for `call_id`), exactly like a
question card. A card opened after a reconnect is rebuilt from the run's journal replay
(`Last-Event-ID`); there is no list route, and an unseen request simply expires.

## Routes

Both are owner routes: they are in `OWNER_ROUTES` (`crates/daemon-api/src/owner.rs`), so a holder of
only the API token (CLI engine, MCP client, script) gets `owner_key_required`; they are under
`PEER_DENIED_FILL_PREFIXES`, so every paired device role is refused for every method; and the
handlers refuse a non-local actor again.

`POST /v1/secret-requests/{call_id}` body (write-only, never echoed, `Debug` prints `<redacted>`):

```json
{ "value": "...", "origins": ["https://example.com"], "programs": [] }
{ "username": "...", "password": "...", "origins": ["https://example.com"], "programs": [] }
```

- Fields must match the request (`value` for `secret`; `username` + `password` for `login`), all
  non-empty. Wrong shape, a bad origin or a store failure answers 400/503 and **keeps the request
  waiting** so the owner can correct the form; a half-written `login` is rolled back.
- `secret_request_gone` (404): answered, cancelled, expired or the run ended. A request is claimed by
  the first caller; a second submit or a cancel finds nothing.
- `secret_request_exists` (409): the name exists and the bot did not ask to replace it.
- 200 `{ call_id, name, outcome: "stored", run_notified }`. `run_notified: false` means the run
  stopped in the same instant; the value is stored and stays stored.

`POST /v1/secret-requests/{call_id}/cancel` answers `{ outcome: "cancelled" }`.

New error codes (with texts in en, tr and de): `secret_request_exists`, `secret_request_gone`.

## Threat model

Assets: the owner's credentials, and the owner's belief that a card is the app's and not the model's.

### Phishing by prompt injection

A page, mail or file can tell the model "ask the owner for their bank password". Mitigations:

- The card is drawn by the app from fixed fields; the model supplies only `name`, `purpose`, `fields`,
  `replace`. `purpose` is length-limited plain text; the card must show it as inert text under a
  fixed heading that says it was written by the bot.
- `fill/` names only: never provider, MCP, e-mail, Composio or OAuth keys, and no way to name
  another namespace.
- **Taint.** The event carries `untrusted_content` (the run, or its root run, is in
  `router.tainted_runs`, or its parent chain cannot be read: fail closed). The card must then show a
  stronger warning ("This run has read content from outside. Something there may have asked for
  this. Do not enter a password you would not give that content directly.") and must not preselect
  anything. Independently of that, the call itself goes through the permission engine: in a tainted
  run an allowed call becomes an ask, so the owner first sees an approval card and then the form.
- Per-run limit: `LoopConfig::max_secret_requests_per_run`, default 3. Refused and cancelled requests
  count, so a looping model cannot nag.
- Global switch: `LoopConfig::secret_requests`, **off by default** until the desktop card ships
  (without a card a request would park the run until it expires). Turn it on with
  `secret_request = true` under `[experimental]` in `host.toml`, or `BOT_SECRET_REQUESTS=on` (the
  environment wins; `off` forces it off). Off: the tool is not offered and the loop refuses it too.
  `praxi doctor` shows the state (`secret_request`). A Settings toggle comes with the card.
- A submit that overlaps the timeout is never reported as "nothing was stored": once the form is
  claimed the timer cannot end the wait, and the model hears `stored` (or the failure) truthfully.
  A login is written all-or-nothing: on any failure the previous username and password are restored.
- The owner decides where the entry may be typed (origins, programs). A value stored this way is not
  usable anywhere until the owner adds a place; with none, `secrets_list` says "nowhere yet".

### Only the owner at the keyboard

- The loop refuses unless the run's initiator is the owner at the desktop (`Human`). Telegram and
  other channels, schedules and routines, subagents and external agents are refused up front with a
  clear message to the model; nothing is shown.
- A run that a paired device started also counts as an owner run for the loop, but its events are not
  shown to the device and the submit route is local-only, so the request cannot be answered and
  expires after `secret_request_timeout` (default 5 minutes, never longer than the approval
  timeout). This is a known gap in "fail fast": the refusal is by timeout, not up front, because, as far
  as the loop can tell, the run does not record that it came from a peer (see "Not solved here").
- A headless host without the owner's desktop has no client that renders the card, so the same
  timeout applies; the daemon cannot tell the two cases apart.
- The submit route needs the owner key (desktop only; headless hosts keep theirs in a 0600 file that
  the card never reads) and a local actor.

### No read-back, no silent overwrite

- No route, tool or event returns a value. The only read path stays `FillSecrets::resolve_value`,
  used by the fill tools after their own approval.
- Replacing needs the bot to say so (`replace: true`) **and** the card to say so
  (`replaces_existing`, shown as "This replaces the stored value for <name>"). The tool refuses an
  existing name without the flag, and the route checks again at submit time (a name may have been
  created while the card was open). Without the flag the only way to change a value stays the owner's
  own `PUT /v1/fill-secrets/{name}`.
- A request is single-use (claimed by its first answer).

### Timeout and cancel

- `secret_request_timeout` (default 5 minutes), also bounded by the approval timeout when that is
  set. Expiry, Cancel, a second tab answering first and a stopped run all end the wait; the
  approval row is marked `expired` or `declined`; the registry entry is always removed.
- Nothing is stored before the owner submits. A cancel or timeout leaves no entry, no allow-list and
  no grant.

### Leak channels (checklist)

Every channel below is covered by a design rule; #220 proves them with canaries.

| Channel | Rule |
|---------|------|
| Tool result to the model | `stored` / `cancelled` and the name only. |
| Events and SSE | `secret_requested` has no value; the answer has no event. |
| Run journal / steps | Same; the card JSON the tool returns has no value either. |
| Audit log | `secret_request.requested / submit / stored / cancelled / refused`: names, flags, origins, programs, never a value (rows are written before the store write). |
| Logs and errors | `SubmitSecretRequest` has a redacting `Debug`; no error message includes a value; the submit route parses the body itself and its 400 for a malformed body never quotes it (serde's own messages would). |
| Redactor | The store's value hook registers the value, with encoded variants, in the same call that stores it, before the run is woken. |
| History / memory / exports | The value is never in a message or tool result, so nothing derived from them holds it. |
| Screenshots, live view, accessibility tree | **Card contract:** the card must hide itself from live view and capture where the platform allows and must mark password inputs so they are masked in the tree. Until the card exists this is open (see below). |
| Clipboard | **Card contract:** no copy button, `autocomplete="off"`, and the card must not read the clipboard. Pasting by the owner is allowed. |
| Browser fill later | Unchanged: `secret` fills are approved per use, bound to the live origin, and scrubbed. |

## Contract for the desktop card (slice 3)

The card MUST:

1. Render only from a `secret_requested` event of a run the owner is looking at, as an app-owned
   modal or card that is visually distinct from chat content. It is not part of the message text.
2. Show: the bot and run, `purpose` as plain text (never markdown, never linkified) labelled as written
   by the bot, the entry `name`, what will be stored (one secret or username + password), and where
   the value is going ("the vault on this computer, as `fill/<name>`; the bot never sees it").
3. When `untrusted_content` is true, show the stronger warning described above, and require an extra
   confirm click before the form can be submitted.
4. When `replaces_existing` is true, say so in words, naming the entry.
5. Let the owner confirm or edit the origins and programs the entry may be typed on. Start empty;
   never prefill from the purpose or any model text. Send them as `origins` and `programs`.
6. Mask password and secret inputs, never echo the value anywhere (no logging, no error toast, no
   state kept after submit; clear the input on submit, cancel, close and error), set
   `autocomplete="off"`, offer no copy and no reveal-and-copy.
7. Post to `POST /v1/secret-requests/{call_id}` with the owner key (`apps/desktop/ui/src/lib/ownerKey.ts`);
   Cancel posts to `.../cancel`. On 400/503 keep the form and show `humanError`; on
   `secret_request_gone` close the card with that text; on `secret_request_exists` show it and keep
   the card.
8. Close on `tool_call_completed` for the `call_id` and when `expires_in_secs` has passed.
9. Be operable with the keyboard only (focus moves to the card, a focus trap, Escape cancels,
   Enter on the last field submits, labels on every input, the warning and `purpose` associated with
   the form) and read sensibly by a screen reader (the heading says who is asking).
10. Be hidden from live view and screen capture where the platform offers it.
11. All strings in en, tr and de.

## Not solved here (and why)

- **Fail-fast for peer-started and headless runs.** As far as the loop can tell, the run does not record that it was started by a
  paired device, so those runs time out instead of being refused up front. Fixing it needs a run
  origin flag; left for a follow-up.
- **Screen capture and live view of the card** need the card; they are part of slice 3 and #220.
- **A Settings toggle** for the global switch; until then it is the environment variable.
- **A list route for open requests**; the journal replay covers reconnects.
- **Audit rows for the model's `requested` for #222** exist (names only); the shape of the shared
  audit view is #222's.
