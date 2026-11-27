# Google Workspace: Gmail, Drive and Calendar with your own Google client

Praxi can connect to Gmail, Google Drive and Google Calendar through **Google's
own MCP servers**. Your mail, files and events go only between Google and the
computer where Praxi runs. For a one-click path that needs no setup, use the
Gmail, Drive and Calendar tiles in the main [Apps](apps.md) list (they go
through Composio). This page is the other path.

Open **Apps → Advanced → Google apps with your own Google client**. Each app is
its own card (**Gmail**, **Google Drive**, **Google Calendar**).

## Why you create a Google client yourself

Praxi has no shared Google login. A shared one would have to pass Google's app
verification (a review of the app, its privacy policy and the Gmail permissions)
and every person would see an "unverified app" warning. A client you create in
**your own** Google account is used by you alone, so none of that applies.
**Short answer: you do not need Google's verification for your own client; a
shared client would.**

## Set it up (about five minutes, once)

Press **Connect** on a Google card. A guide opens with the exact pages of the
Google Cloud console; each button opens the page in your browser.

1. **Create a project.** Any name, for example `Praxi`. Use an existing one if
   you have it.
2. **Turn on the services.** Tick the apps you want, then open each listed page
   and press **Enable**. Every app has two: its API (`gmail.googleapis.com`,
   `drive.googleapis.com`, `calendar-json.googleapis.com`) and its MCP service
   (`gmailmcp.googleapis.com`, `drivemcp.googleapis.com`,
   `calendarmcp.googleapis.com`).
3. **Branding.** An app name (for example `Praxi`) and your e-mail as support
   address.
4. **Audience.** Choose **External** (or **Internal** if your Google Workspace
   allows it) and add your own Google address under **Test users**.
5. **Data Access.** Add the read-only permissions the guide lists (there is a
   **Copy the permissions** button).
6. **Create the client.** Type **Desktop app**. Copy its **Client ID** and
   **Client secret** into the two boxes and press **Save and sign in**.

Praxi stores the secret in its secret store (never in `mcp.toml`, never shown
again, never given to a bot) and keeps the Client ID with each Google app's
settings (it is not a secret: it is part of the sign-in page's address).
The other Google apps reuse the same client, so you type it only once. The
sign-in page opens in your browser; afterwards the card appears under
**Connected**.

When the sign-in finishes, Praxi runs a **Test** for you: one fixed read-only
call that shows the first three items (three recent mails, three recent files,
or three calendars). You can repeat it any time under **Manage → Test: show the
first items**. The answer is shown only to you; it is never given to a bot.

## Read-only first

Praxi asks Google only for read-only permissions (for example
`gmail.readonly`, `drive.readonly`, `calendar.events.readonly`) and lets only
the matching read tools through. A bot cannot send mail, delete or share files
or change events with these cards. Reading still follows your usual approval
rules ([Security model](security.md)): a Google app starts as one whose labels
are **not** trusted, so each action asks first unless you change that under
**Manage**.

## Praxi on a server

If Praxi runs on a server, sign in the same way. The page opens in your browser,
the desktop app catches the answer and passes it to the server; see
[Connecting apps](apps.md#oauth-sign-in-is-stuck-on-waiting-for-the-browser-sign-in).
Google accepts a loopback address with any port for a **Desktop app** client, so
nothing has to be registered for the server.

## What can go wrong

- **"Google hasn't verified this app."** Expected for your own client. Choose
  **Advanced**, then continue. Only you can sign in to it.
- **Signed out again after about a week.** While the app is in **Testing**,
  Google can end the sign-in after about 7 days. Publishing the app in the
  Google console avoids this (it still stays yours alone). The exact rule is
  Google's and can change.
- **"Access blocked" or an administrator message.** A Google Workspace
  administrator can block apps like this. Ask them to allow it.
- **The Test says Google refused.** The API or MCP service is not enabled in
  your project, or a permission is missing under **Data Access**.
- **Lost the client secret.** Create a new one in the Google console and use
  **Use a different Google client**.
- **"This computer is not yet confirmed as an administrator."** On a server,
  signing in is something only the server's administrator desktop may do; see
  [Remote host](remote-host.md).

## Sign out and remove

**Manage → Sign out** forgets the stored sign-in on the computer where Praxi
runs; **Disconnect** removes the app. To revoke Praxi's access on Google's side
as well, open your Google account's **Security → Third-party access** and remove
the client you created.

All Google apps share one client secret. **Disconnect** offers **Also forget the
saved client secret**; it is ticked when no other Google app uses the secret, and
the secret is only deleted when none does. Cancelling or timing out a first
sign-in removes the half-made app and its secret the same way. To rotate the
secret, create a new one in the Google console and use **Use a different Google
client**: it replaces the stored one for every Google app.

The page you sign in on is shown in the sign-in card. If Praxi is sent to a
site other than the app's own (for Google: `accounts.google.com`), it does not
open the page by itself; it says where the page is and opens it only when you
press **Open it anyway**.

## In `mcp.toml`

The cards write ordinary server settings; you can write them yourself:

```toml
[[server]]
id = "google-gmail"
tool_allow = ["search_threads", "get_thread", "get_message", "list_labels"]

[server.transport]
type = "http"
url = "https://gmailmcp.googleapis.com/mcp/v1"

[server.oauth]
client_id = "123456789012-abc.apps.googleusercontent.com"
client_secret_ref = "mcp_google_client_secret"   # a secret NAME
issuer = "https://accounts.google.com"
scopes = ["https://www.googleapis.com/auth/gmail.readonly"]
offline_access = true                            # a refresh token, so it stays signed in
```

Store the secret with `bot secrets set mcp_google_client_secret`. See
[MCP servers](mcp-servers.md).
