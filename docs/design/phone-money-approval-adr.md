# ADR: Approving money asks from the phone

Status: accepted. Builds on [Telegram approval cards](telegram-approval-cards-adr.md) and the
[threat model](threat-model.md). Code: `crates/daemon-api/src/phone_approval.rs`,
`crates/agent-core/src/approval.rs` (money flag and checkout facts),
`crates/builtin-tools/src/browser/payment.rs` (checkout facts),
`crates/channel-telegram/src/bridge.rs` (the link on the money card).
Guide: [Telegram](../guide/telegram.md#approving-a-payment-from-your-phone).

## Context

Money is the one thing the Full level still asks about. The Telegram card for a money ask
has no buttons (it is "high risk"), so when the owner is away from the desktop a mission item
that reaches a checkout waits until it is parked. The owner wants to decide from the phone.

A Telegram chat is a weak credential: the account can be stolen, the chat can be read by
someone holding the phone unlocked, a card can be forwarded, and Telegram's servers see
everything. The Telegram ADR therefore keeps every high-risk decision on the desktop. This ADR
adds a path for money that keeps that property: **what Telegram carries is never enough to
approve a payment.**

## Assets

- The owner's money (a card on file, a payment account, a crypto wallet).
- The decision on one specific pending money ask (`call_id`), and nothing wider.
- What the checkout page shows (merchant, amount, possibly a delivery address).

## Actors and what they hold

| Actor | Holds | Must not be able to |
| --- | --- | --- |
| Owner at the desktop | owner key (app) | (decides as today) |
| Owner on the phone | the link from Telegram + the phone-approval PIN | approve anything but the one ask the link names |
| Thief of the Telegram account / reader of the chat / Telegram itself | the link (nonce) | approve, or see the checkout screenshot |
| Someone who can reach the tunnel address | nothing, or a leaked link | guess a nonce, brute-force the PIN |
| The bot / a web page it reads | can write text into the card (merchant, amount) | mint links, choose the PIN, reach the page's decision with a forged form |
| A token-only API caller (CLI engine, MCP client) | the general API token | mint links, read or change the PIN |
| A paired device (desktop peer, phone, node, host) | its peer role | anything under `/v1/phone-approval` (refused like incoming webhooks) |
| An administrator desktop of a host | the host's administration role | mint a link or open the page (it may turn phone approval on or off for the host, as it administers the host's other settings) |

## Decision

1. **Second factor: a PIN, mandatory.** The owner turns phone approval on in the app (Settings
   > Notifications) with a 6 to 12 digit PIN and the public address of a tunnel they choose. The
   PIN is stored only as an Argon2id hash (random salt) in `phone-approval.json` in the data
   dir. Without a PIN the feature is off and money stays on the desktop exactly as before.
   The PIN never travels through Telegram: the owner types it on the page. Passkeys are a
   follow-up (they need an https origin and a registration ceremony); a code shown in the app
   was rejected because the owner it helps is away from the app.
2. **Telegram alone cannot approve money: enforced in layers.**
   - The agent loop never labels a money card `risk: low`, and the Telegram and Discord
     bridges give a card with `money_movement` no button whatever its label says.
   - The Telegram card for a money ask still has no callback buttons. It shows a link to the
     page and says the PIN is needed. A forged or replayed callback for a money `call_id` finds
     no pending entry in the bridge and is refused (existing rule). Discord keeps "decide in
     the app" for money (no link yet; it would reuse the same endpoint).
   - The daemon refuses `approve` on `POST /v1/approvals/{call_id}` for a money ask when the
     request says it comes from a chat channel (`x-praxi-channel`, set by the Telegram and
     Discord clients): `403 phone_approval_required`. This is defence in depth against a
     future bridge bug, not a boundary against code that already runs in the daemon with the
     owner key (out of scope: such code can do anything the owner can).
   - The page itself never accepts a decision without the PIN in the same flow.
3. **The link is a one-time nonce, bound to the exact ask, 10-minute TTL.**
   - Only the owner key can mint (`POST /v1/phone-approval/links`, owner route; the Telegram
     bridge holds the owner key). A link is minted only for a pending approval whose card is
     a money ask (`money_movement`), never for any other card.
   - The nonce is 32 random bytes (base64url). The daemon keeps only its SHA-256, the
     `call_id`, a SHA-256 digest of the ask (`call_id`, tool, summary, input, details), the
     expiry (mint time + 10 minutes) and the attempt count. Minting again for the same ask
     replaces the old link.
   - Every use re-reads the pending ask and recomputes the digest; a different digest, a
     decided or expired ask, or a past expiry refuses and burns the link.
   - **Single use.** The decision step removes the entry under the lock *before* it calls the
     agent loop, so a replayed or concurrent POST finds nothing. A failed decision does not
     put it back.
   - Links live in memory: a restart drops them (pending asks do not survive a restart either).
4. **The page.** Served by the daemon on the loopback listener, reached through the owner's
   tunnel (like [incoming webhooks](../guide/incoming-webhooks.md)): no new port, the Host
   check stays loopback-only, so the tunnel must rewrite `Host`.
   - `GET /v1/phone-approval/p/{nonce}`: merchant, domain, amount (as the page states them,
     labelled so) and a PIN field. No screenshot yet.
   - `POST …/{nonce}/unlock` (form `pin`): a right PIN marks the link unlocked and returns the
     full page: the checkout screenshot, the facts and **Approve** / **Deny**, each carrying a
     fresh 32-byte confirm value that only this response contains. The screenshot is never
     shown before the PIN, so a Telegram thief cannot read the checkout page either.
   - `POST …/{nonce}/decide` (form `confirm`, `decision`): needs the unlocked link and the
     matching confirm value (constant-time compare); approves **once** (never a standing
     grant) or denies, then the link is gone.
   - Headers: `Cache-Control: no-store`, `Referrer-Policy: same-origin`, a CSP without
     scripts (`default-src 'none'; img-src data:; style-src 'unsafe-inline'; form-action 'self';
     frame-ancestors 'none'`), `X-Frame-Options: DENY`. The page has no JavaScript.
   - A browser POST carries `Origin`. The app's origin check stays; on these three paths only,
     an `Origin` equal to the configured public address's origin is also accepted.
5. **Brute force.** A wrong PIN costs the link one of 3 attempts (then it is burned) and counts
   towards a daemon-wide limit: 5 wrong PINs within 15 minutes lock phone approval for 15
   minutes (the app shows the lock). With a 6-digit PIN that is at most 480 guesses a day.
   Unknown or expired nonces get the same "this link no longer works" page as burned ones.
6. **Checkout facts.** When a money ask comes from the managed browser, the browser tool reads
   the page right before the card is built: domain (from the browser's own URL), merchant (the
   page's `og:site_name` or title), amount (the first visible total-like text) and a viewport
   screenshot. Text facts go into the card's details (shown in the app too, capped and
   sanitised); the screenshot stays in memory in the agent loop next to the pending ask and is
   dropped when the ask closes. **Merchant, amount and the screenshot come from the page and
   can lie**; the page says "as the checkout page shows it". The domain is the browser's.
   Money asks that are not a browser page (a payment CLI, a payment app action) show the
   card summary instead.
7. **Audit.** Every mint, unlock failure, lock-out, approval and denial from the phone is an
   audit row (`phone_approval.*`) with the `call_id`, never the nonce or the PIN.
8. **What stays as it is.** The owner key, its owner-route table and the paired-device rules
   are unchanged; the new owner routes are added to the table. Ordinary peers are refused the
   whole `/v1/phone-approval` tree; an administrator desktop may read and change the settings
   but is refused minting a link and the page (`admin_desktop_never`). Low-risk Telegram
   buttons, the desktop card and the 10-minute Telegram card TTL do not change.

## Residual risks

- **Phishing.** Someone who controls the chat can send a fake link to a look-alike page and ask
  for the PIN. The guide tells the owner to check the address; passkeys (origin-bound) would
  close this and are the follow-up.
- **A leaked PIN plus the chat.** Both factors together approve one payment. Change the PIN in
  the app (it drops every open link) or turn phone approval off.
- **The tunnel.** The tunnel provider sees the nonce in the path and the PIN in the POST
  body unless the tunnel is end-to-end TLS to a host the owner controls. The nonce alone is
  useless; the PIN is the owner's to protect. Use an https tunnel.
- **Lying pages.** A checkout page can show a different amount than it charges. That is true on
  the desktop card too; the domain is the reliable fact.
- **Code inside the daemon with the owner key** (an unrestricted shell on the same OS account)
  can approve anything already; this ADR does not change that standing.

## Alternatives rejected

- **Telegram buttons plus a PIN typed into the chat.** The PIN would sit in the chat history,
  on Telegram's servers.
- **Making the phone a paired device.** Pairing grants a standing role; a money decision should
  be one ask at a time, and the paired-device rules were to stay unchanged.
- **Long-lived or reusable links.** Replay and forwarding risk.
- **Showing the screenshot before the PIN.** It would hand the checkout page (address, name) to
  whoever reads the chat.
