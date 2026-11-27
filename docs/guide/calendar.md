# Calendar (CalDAV)

Bots can read, add, change and delete events in a CalDAV calendar (Nextcloud,
Radicale, Fastmail, iCloud, Synology ...), find free time across calendars, make
repeating events and invite people. Every call goes through the permission engine
like any other tool.

## Set it up

Open **Settings > Mail & web > Calendar**:

1. Pick your service (iCloud, Fastmail, Nextcloud, Google or Other). The address is filled in for you.
2. Enter your user name and an **app password** (not your normal password). Each
   preset explains where to create one.
3. Press **Test connection**. It lists the calendars it finds and tells you whether
   the service sends invitations itself. Pick the calendar your bots should use.
4. Press **Add calendar**.

The password goes to the secret store; it is never written to a file and never shown
again. The tools work from the next call, with no restart. Remove a calendar with the
bin icon next to it, which also deletes the password Settings saved for it (a secret
you referenced by hand in `calendar.toml` is left alone: something else may use it).

Google only accepts app passwords for calendars on some accounts. If the test fails
for a Google account, that is the likely reason.

You can still configure calendars by hand with `<data dir>/calendar.toml` (`name`,
`url`, `username`, and `password` as a secret-store reference such as
`calendar/home/password`); the Settings page edits the same file.

## Tools

| Tool | Effect | Notes |
| --- | --- | --- |
| `calendar_list_events` | read | Events overlapping a range; repeating events are listed once per occurrence. Titles and notes are untrusted text. |
| `calendar_free_busy` | read | Free slots across all (or the named) calendars, optionally inside working hours. Reports a calendar it could not read instead of hiding it. |
| `calendar_create_event` | write | Adds an event, optionally repeating (`recurrence`, an RRULE) and with `attendees`. |
| `calendar_update_event` | write | Renames or moves an event, changes its place, notes, repeat rule or attendees. Only the fields you pass change. |
| `calendar_delete_event` | destructive | Deletes one event by `uid`. It cannot be undone, so the lower permission levels ask first. If you organise the event and the server cancels it for the attendees, they are told. |

Dates are `YYYY-MM-DD` (whole day) or RFC 3339 with an offset.

### Invitations

Adding attendees, changing an event that has attendees (any change, a note
included: the server tells everyone), or removing one sends mail to other people, so
it goes through the same approval step as sending an e-mail. The lower permission
levels ask you first; **Full access** lets a trusted run through (it still asks before
spending money). Every one is recorded in the audit log.

If the calendar server supports CalDAV scheduling it sends the invitations itself.
Otherwise the bot sends an e-mail with an `.ics` attachment through your e-mail account
(set one up under Settings > Mail & web > E-mail). An event without attendees notifies
nobody. Your outgoing-mail rules apply either way, and before anything is written: an
address on your opt-out list or your daily limit stops the change, and the send log
shows each invitation with the bot that sent it.

## Safety

- The URL must be `https` (plain `http` only for this computer: `localhost`, a `*.localhost`
  name or a loopback address such as `127.0.0.1` or `::1`, e.g. a test server).
- Addresses in cloud-metadata and link-local space (for example `169.254.169.254`) and the
  daemon's own port are never contacted, whatever name leads there. Your own network is
  fine, so a calendar on a home server works.
- A server may send the daemon to a calendar at another address only on the host you
  entered (iCloud, which spreads calendars over its own hosts, may move between them).
  Anything else, a login in the address, or another port or scheme is refused.
- A server answer is read up to 4 MB and a listing stops at 10,000 events, so a hostile
  server cannot exhaust memory or time.
- The saved password is only ever sent to the address and user name it was saved with.
  Changing either one means typing the password again.
- Redirects are never followed, so the password cannot be sent to another host.
- A server-supplied link is used for changing or deleting only if it stays inside the
  configured calendar.
- Calendar text returned to the model is marked as untrusted data; a run that
  read it is tainted like one that read a web page or an e-mail.
- Adding, testing and removing calendars is an owner action: a paired desktop cannot
  change the credentials of a host.
