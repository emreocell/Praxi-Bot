# Secure credential requests

[Issue #422](https://github.com/emreocell/Praxi-Bot/issues/422) builds on the
[fill-secret vault](../guide/security.md#secrets-the-assistant-may-type-fill-secrets)
and the [desktop deep-link contract](desktop-deep-links-adr.md). A bot can ask
the owner for an existing login, API key, or backup code. Payment credentials
are out of scope.

## Request and response

1. The model calls `secret_request` with a vault entry `name`, an exact HTTPS
   `origin`, and a one-line plain-text `purpose` of at most 200 characters. The
   optional `fields` picks a single secret or a login pair. The tool accepts
   request metadata only.
2. The daemon requires a human run with messages authenticated as the local
   desktop owner. It applies the ordinary write and taint gates, caps requests
   per run, and records an audit entry before parking the run and emitting the
   form event. The desktop renders the bot, reason, origin, and expiry from
   this event. Pending requests also appear under Needs you.
3. The app shows a local notification when its notification preferences allow.
   Paired Telegram accounts and the paired Discord owner get a private notice
   with a link to Praxi's static launcher. The launcher validates the chat
   id in the HTTPS fragment and opens `praxi://chat/<uuid>` on the owner's
   click. It has no form or external assets. The link navigates to the chat
   in the selected desktop session. The notice contains no purpose, origin,
   vault entry name, or form id.
4. The desktop posts just the form fields over the verified local connection
   to `POST /v1/secret-requests/{call_id}` with the owner key. The daemon
   claims the request once, records the submission audit, and writes to
   `fill/<name>` (or `fill/<name>/username` and `fill/<name>/password`).
   The stored metadata has the requesting bot id, canonical HTTPS origin and
   displayed purpose. The vault's value hook registers both login fields
   with the redactor during the write.
5. The run wakes with the entry name and `stored` or `cancelled`. The bot
   uses `secrets_list` and the named `secret` parameter of `browser_fill`.
   The browser verifies the live field frame and top-level origins again
   before typing. A requested entry starts with approval required on every use.

The owner can cancel with `POST /v1/secret-requests/{call_id}/cancel`. A
request expires after five minutes by default, capped by the approval timeout.
Cancel and expiry of an unclaimed form leave the vault untouched. A submit
claimed before the deadline finishes even if the HTTP connection closes. A
failed write restores the claim for retry while the request is live. Closing
or stopping an unclaimed request removes it. A stop after submission was
claimed can end the run independently of the authorized vault write.

## Trust boundaries

- **Model and untrusted content.** The app owns the form. It renders the
  model's purpose as inert text and shows the exact HTTPS origin separately.
  Tainted runs keep the ordinary approval gate and the form's additional
  confirmation checkbox. The vault enforces destinations and bot ids.
  Purpose records the reason the owner saw. It is descriptive metadata.
- **Owner and peers.** The default applies to the desktop-launched daemon
  with a vault. Headless startup, remote peers, channel-started runs and
  bearer-only API calls lack this authority. `BOT_SECRET_REQUESTS=off` or
  `secret_request = false` under `[experimental]` in `host.toml` disables
  requests. Peer routes cannot list or answer them. A link grants no
  authority and carries no credential.
- **Replacement.** The bot must ask explicitly and the app labels the
  replacement. The vault checks under its write lock that every existing
  entry belongs exclusively to the requesting bot and origin, with no
  program scope or registered-account record. Replacement clears standing
  grants for that entry. The owner can manage other entries in the Vault.
- **Leaks.** Both login fields are masked and cleared from the DOM as soon
  as the owner submits or cancels. HTTP errors never echo the body. The
  route holds parsed fields in zeroizing wrappers and writes values only
  through the vault. Secure requests reject fields shorter than the redactor
  can mask. The form requires six characters per field. Audit rows and events carry request metadata. The
  transcript, run journal, and model receive the result and never a value
  from the form. Legacy journal events without an origin cannot open an
  editable form. Older vault entries that already have a login identifier
  in metadata retain it until the owner edits or replaces the entry.
- **Platform limits.** Desktop capture protection uses the existing app
  window shield, with the platform limits described in the security guide.
  A process running as the local OS user and process memory in swap or crash
  dumps sit outside this boundary. Opening a channel's link requires a
  published launcher and a browser and OS that hand the custom URI to the
  installed desktop app.
  The launcher page is served from Praxi's GitHub Pages site, which the notice
  link trusts: a compromised page could show a false "open in Praxi" prompt,
  but it can only open `praxi://chat/<uuid>`, so the real form is still the
  desktop's and no value is ever entered on the page.

## Mission integration

In a mission session nobody waits for the form, so the request must not hold a run:

1. The same gates apply (switch, per-run cap, taint, exact destination, audit
   `requested`). A mission session is never a `Human` run, so the owner-run check does
   not apply to it; the form it opens is exactly as owner-only as any other.
2. The ledger item being worked on (the one `doing`) is parked as `blocked` with a
   reason that names the entry. With no single `doing` item nothing is opened and the
   model is told to park the item itself.
3. The daemon registers the request with that item as its target and emits
   `secret_requested` with `parked: true`. The model's tool call returns at once with
   "parked as blocked, take the next open item". The session moves on; other items run.
4. The form stays open for 24 hours (`mission_secret_request_timeout`), not five
   minutes, because the owner may be away. The card stays open after the tool call has
   completed and after the session ended.
5. Submit stores the value exactly as above (bot, origin, purpose binding) and then
   releases only the target item to `todo` with an owner answer that names the entry
   and says how to use it (never a value). The mission goes back to `running` if it was
   `blocked`. Cancel and expiry store nothing and leave the item parked; the owner can
   still add the entry in the Vault and release it from the blocked list. Audit rows:
   `requested`, `parked`, `submit`, `stored`, `cancelled`, `expired`.
6. A request is refused (`secret_request_gone`) once its mission is done, failed or
   cancelled: no value is stored for a mission that is over.
7. Requests live in daemon memory. After a restart the form is gone and the item stays
   parked with its reason; the owner adds the entry in the Vault and releases it.
