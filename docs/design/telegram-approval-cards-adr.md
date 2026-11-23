# ADR: Telegram approval cards for low-risk asks only

Status: accepted. PR: #645 (closes #585). Code: `crates/agent-core/src/approval.rs`
(the `risk` label), `crates/channel-telegram/src/bridge.rs`.

## Context

Telegram shows a short card; it cannot show a full diff, a command line or the
page the bot wants to act on. Letting a phone approve anything would turn a
stolen or shared chat into a remote control.

## Decision

- The daemon labels every approval `risk: low|high` in its details. **Low** means
  an ordinary policy ask, effect not Destructive, started by a human, not on
  the owner's PC or browser, and no computer target. Everything else is high,
  and a missing label counts as high.
- High-risk cards carry no buttons and say the desktop app is needed.
- Low-risk cards offer **Allow once** and **Deny** only. There is no "Always"
  from the phone.
- A button press atomically takes the pending entry before the daemon call, so
  a replayed callback finds nothing. A refused or failed press puts it back so
  retry works.
- Only the paired owner chat can press. A press from another chat is refused
  and does not use the card up.
- A card stops working after 10 minutes (`APPROVAL_CARD_TTL_MS`); the desktop
  can still decide.

## Consequences

- Quick yes/no on the phone for harmless asks; dangerous ones stay on the desktop.
- The default is the safe side: anything not labelled low is treated as high.
- Telegram strings exist in en and tr only (the crate has no de locale).

## Alternatives rejected

- Buttons on every card: the phone cannot show enough to decide safely.
- Offering "Always allow" on the phone: a standing grant should be a deliberate
  desktop action.
- Long-lived or reusable buttons: replay and forwarded-card risk.
