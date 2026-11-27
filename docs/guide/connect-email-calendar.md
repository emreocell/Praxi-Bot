# Connect my email and calendar

One guided flow connects your mail and your calendar. You type your address, Praxi
picks the easiest way that works for your provider, tests it live, and tells you in
plain words what to do if something fails.

Open it in **Settings → Connections → Mail and web → Connect my email and calendar**.

## The short version

1. **Your address.** Type it. Praxi reads the provider from it (Gmail, Outlook,
   iCloud, Yahoo, Fastmail or another one). If it guessed wrong, or it is a work address
   on Google Workspace or Microsoft 365, pick the right provider from the list.
   Choose whether the bot may see your **mail**, your **calendar**, or both.
2. **The easiest way for that provider** (see the table below). For an app password,
   the page shows two lines of instructions and a button that opens the exact page
   of your provider. You paste the password, pick the bot that may use the mailbox,
   and press **Test connection**.
3. **The live test.** Praxi logs in to the mail servers and, if you wanted it, to the
   calendar. Nothing is saved and nothing is sent. When it works, press **Connect**.
   If only one of the two works you can **Connect what works**.

## What each provider uses

| Provider | Mail | Calendar | Why |
| --- | --- | --- | --- |
| iCloud | App-specific password | Same password (CalDAV) | One password from your Apple account page works for both. |
| Fastmail | App password | Same password (CalDAV) | Make one that allows Mail and Calendars. |
| Gmail / Google Workspace | **Sign in with Google** (Google's own Gmail server, read-only), or an app password (mail only) | Sign in with Google (read-only) | Google does not accept app passwords for calendars. The sign-in uses the Google client you make once in your own account, see [Google Workspace](google-workspace.md). |
| Outlook / Microsoft 365 | Microsoft sign-in in the Apps window (Outlook) | Same | Microsoft turned off app passwords for mail, so a password cannot work any more. |
| Yahoo | App password | Not offered | Yahoo has no calendar connection Praxi can use. |
| Anything else | App password (servers are guessed as `imap.<domain>` and `smtp.<domain>`) | CalDAV, from the server address you type | Open **More options** to correct the server names, then confirm them. |

The Google and Microsoft sign-ins happen in the **Apps** window (the wizard opens it for
you). When you are done there, come back and press **I am done, check**.

### Calendars

You type only the address of the calendar server (iCloud and Fastmail are filled in
for you). Praxi finds your calendars from there. If it finds several, tick the ones the
bot should see; each becomes its own calendar in
[Calendar (CalDAV)](calendar.md). Praxi never follows a calendar server's redirect to
another address (for your safety), so for another provider type the final CalDAV
address the provider lists.

### Servers Praxi does not know: you confirm where the password goes

For Gmail, iCloud, Yahoo and Fastmail Praxi uses the provider's own servers and the test
starts at once. For **any other provider**, or if you change the server names or ports, the
server names are only a guess (or something you typed), so Praxi first shows them: the
incoming (IMAP) server, the outgoing (SMTP) server and the calendar (CalDAV) address, each with
its port. Nothing is tested, and your app password is not sent anywhere, until you tick
**These are my provider's servers** (changing any of them takes the tick back).

Connections are always encrypted. Mail uses port 993 for incoming and port 465 (secure
connection) or port 587 (STARTTLS, only if you pick it) for outgoing; a calendar address must
start with `https://`. Server names must be public names, not an IP address or a name inside
your own network, and Praxi's daemon refuses a server that points at this computer or a private
network. To connect a mail or calendar server on your own network, use the
[E-mail](email.md) and [Calendar (CalDAV)](calendar.md) settings instead.

## What the bot may do: read first, sending is a second opt-in

- A new mailbox is **read and draft only**: the bot can read and search the mail and
  write drafts, but it cannot send anything.
- **Also let the bot send mail** is a separate, unticked box. When you tick it, each
  message is shown to you first, unless you gave that bot **Full access**. Your
  [mail guard](email.md) rules (opt-out list, daily limit, send log) apply either way.
- A calendar login (an app password) can change events as well as read them. Adding or
  changing events follows the bot's access level, like sending mail. Anything that
  invites other people goes through the same approval.
- A mailbox belongs to **one bot** that you pick. Only that bot can see it.

## Where the secrets go

The app password you paste is sent to the app's own background service and stored in
the secret store (the operating system's keychain or the encrypted file). It is never
shown again, never written to a log, never returned by the API and never put in front
of the model. The status lines of the wizard say only "works" or "does not work". The
Google client secret of the Google sign-in is stored the same way.

## When something does not work

| You see | What it means | What to do |
| --- | --- | --- |
| "The server refused the login" | The password is wrong, or it is your normal password where an app password is needed. | Make a new app password on the provider's page (the button opens it) and paste it again. For iCloud, if your Apple ID is not your iCloud address, type the Apple ID under **More options**. |
| "App passwords need 2-Step Verification" (Google) | The setting is off, or a work account's admin turned app passwords off. | Go back and choose **Sign in with Google**. |
| "We guessed the server names and they do not answer" | Another provider, and `imap.<domain>` is not its server. | Open **More options** and type the server names your provider lists. |
| "The secure connection failed" | The server name or the port is wrong, or the network blocks it. | Check the names and ports under **More options**. |
| "The server did not answer in time" | A slow or busy server. | Try again in a moment. |
| "The calendar server found no calendar at that address" | The CalDAV address is wrong. | Type the address your provider lists for CalDAV. |
| "The calendar server sent us to another address" | The server redirects, which Praxi does not follow. | Type the final calendar address your provider lists. |

## Revoke and disconnect

- Each mailbox and calendar has a bin button in
  **Settings → Connections → Mail and web** ([E-mail](email.md), [Calendar](calendar.md)). It
  deletes the saved password too.
- **Disconnect everything** (on the same **Connect my email and calendar** card) removes
  every mailbox, every calendar and the Google Gmail and Calendar sign-ins in one step,
  after one confirmation. The Google client you made is kept (Drive may use it). A bot's
  own AgentMail address is not touched.
- To take the access back **at the provider** too, open its page (the last step of the
  wizard links it): your Apple account page, Google's
  [third-party access](https://myaccount.google.com/permissions) page, Microsoft's
  [app permissions](https://account.live.com/consent/Manage) page, or the app-password
  list of Yahoo or Fastmail, and delete the "Praxi" entry.

## Notes for the command line

`bot email add`, `bot email test` and the `calendar.toml` file work as before (see
[Email integration](email.md) and [Calendar (CalDAV)](calendar.md)); the wizard writes
the same settings.
