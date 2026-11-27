# Connecting apps (the Apps dialog)

Open **Apps** from the left rail or the command palette (`Ctrl/Cmd+K`).
The main view connects apps through Composio. **MCP servers** opens the
settings for direct, local and custom MCP connections.

## Connect Composio once

At the top of **Apps**, choose **Connect Composio** to sign in in your system
browser, or choose **Use a project API key**. The daemon validates the key and
stores it in its secret store. When Composio supplies the organization or
project name, the account card shows **Connected as** followed by that name.

## Connect and manage an app

1. Find an app in **Discover**. Popular apps appear first. Use search, the
   category filters and **Show more** to browse the catalog.
2. Open the app's page to see its actions and select **Connect**.
3. Finish authorization in your browser. Praxi checks the authorization with
   Composio until it succeeds or expires.

This also works when the bots run on a paired host. The browser opens on the
desktop you use, Composio receives the response, and the host polls Composio
over HTTPS. The browser never needs to reach the host.

Open the app again under **Your apps** to test, disconnect or change bot
access. Praxi displays the account name when Composio supplies an alias or a
read-only test identifies it. **Test connection** checks the bound
authorization. Gmail, Google Drive, Google Calendar and GitHub also have
read-only tests that can show a name or up to three samples to the owner.
Bot access and approval rules apply to the tools exposed by Praxi's existing
Composio integration. Tool inputs and results pass through Composio.

Use **Sign out of Composio** on the account card to suspend access to all
Composio apps. Praxi keeps your app settings, including bot access.

## MCP servers

Choose **MCP servers** in the top bar for direct and self-managed connections:

- **Remote MCP servers:** Install a direct OAuth server such as Atlassian,
  Linear, Notion or Stripe, then finish sign-in in your browser. Manage
  existing remote servers here.
- **Google with your own setup:** a guided setup that connects Google's own MCP
  servers (Gmail, Drive, Calendar) with a client you create yourself. See
  [Google Workspace](google-workspace.md). The Gmail, Drive and Calendar apps in
  the main list use Composio and need no setup.
- **Custom MCP server:** Add a `Stdio` (local command plus optional `KEY=value`
  environment lines) or `HTTP` (server URL) MCP server by hand, and pick which
  bots may use it. See [MCP servers](mcp-servers.md) for the underlying
  `mcp.toml` format and security flags.
- **Local developer servers:** Local stdio servers from the built-in catalog
  that need no API key (such as **Filesystem**, **Playwright**, **Fetch**,
  **Context7**, **SQLite**, and **Google Drive**), along with any local servers
  you have already added. Servers that require a folder (like **Filesystem**)
  ask for the single **Folder the bot may read and write** inline — Praxi
  refuses its own data and credential folders.
- **Services with an access token:** Catalog servers that authenticate with a
  personal API key or token (such as **GitHub**, **Brave Search**, **Sentry**,
  **Todoist**, **Obsidian**, **Home Assistant**, **Exa**, **Tavily**,
  **Firecrawl**, **PostgreSQL**, **Slack**, and local **Notion**). Clicking
  **Connect** opens password fields under the row with a **Get API key** link
  where available. Secrets are saved in your OS keychain and never shown again.

![The Advanced view, starting with the form for a custom MCP server](images/apps/apps-advanced.png)

## MCP server status

A new direct OAuth connection stays in **Signing in** until authorization
finishes. If you cancel or it times out after five minutes, Praxi removes
the unfinished server entry.

Status badges on MCP servers mean:

| Badge | Meaning |
| --- | --- |
| **Connected** | Server session is up and its tools are available to allowed bots. |
| **Connecting** | Daemon is starting the server or establishing the session. |
| **Sign-in required** | Remote HTTP server needs an OAuth sign-in before tools can run. |
| **Disconnected** | Server is not currently connected; use **Reconnect** under **Manage**. |
| **Error** | Last start or connection attempt failed; details appear below the row. |
| **Quarantined** | Server failed repeatedly and was paused until you fix it and press **Reconnect**. |

## Managing connections and signing out

Under **MCP servers**, hover or focus a connected server row and click **Manage**. Rows that need attention (such as `Error`,
`Sign-in required`, `Sign-in expired`, or `Quarantined`) always keep **Manage**
visible.

### Direct OAuth and MCP servers

Inside **Manage** you can:

- **Choose which bots may use the app** under **Who can use this app** and click
  **Save**.
- **Decide whether the server may label its own tools** under **Do you trust
  this server to describe its own tools honestly?** With **No** (the default),
  every action asks first; with **Yes**, only the tools the server marks
  read-only run without asking. See [MCP servers](mcp-servers.md) for how these
  labels are used, and [Connect your apps](connect-your-apps.md#when-an-app-changes)
  for what happens when a tool changes after you approved it.
- **Turn individual tools on or off** under **Tools bots may use** (this writes
  `tool_exclude` in `mcp.toml`). Tools that modify or send/delete data are
  labeled **changes data** or **can delete or send**. If you configured a custom
  `tool_allow` list in `mcp.toml`, Praxi notes that tools are limited by that
  allow-list.
- **Sign out** (HTTP OAuth servers when signed in): clears the stored OAuth
  tokens from the daemon's secret store while keeping the server configured. The
  server moves to **Sign-in required** until you click **Sign in** again.
- **Reconnect** (when `Error`, `Disconnected`, or `Quarantined`): tells the
  daemon to restart or reconnect the server immediately.
- **Disconnect**: asks for confirmation (**Disconnect this server?**), then
  removes the server configuration from `mcp.toml`.
- **Test connection**: starts the server and lists its tools, without running
  any of them (see [Test connection](connection-test.md)).

![The Manage panel of a connected app: bot access, the trust question, tools, and Test connection](images/apps/apps-manage.png)

## Per-bot access

Each Composio app and MCP server can be shared with all bots or restricted to
specific bots. For Composio, open the app page, choose who can use it and click
**Save**. The following controls apply to MCP servers. Connected rows show up to four bot avatars (plus
`+n` when more bots have access) alongside **All bots**, **No bots**, or
**Bots: …**.

You can change access from two places:

1. **From Apps:** open **Manage** on the server row, choose **All bots** or
   **Only these bots**, tick the bots that should see it, and click **Save**.
2. **From a bot's settings:** open the bot's profile (or the new-bot dialog),
   scroll to **Apps**, and toggle each connected server on or off. Unticking an
   app that was open to all bots limits it to your other bots.

A few important rules:

- Selecting **Only these bots** with none ticked (**No bots**, stored as
  `bots = []`) gives access to **no bot**, never every bot.
- Unassigned bots do not see the server's tools and cannot call them.
- If the server's bot access changed elsewhere while your panel was open, Praxi
  refuses to silently overwrite it and refreshes to show the current state.
- Connecting an app and granting a bot access does **not** bypass approvals:
  destructive or write tools still ask for permission (see
  [Security model](security.md) and [MCP servers](mcp-servers.md)).

## Troubleshooting a stuck or failed connection

### OAuth sign-in is stuck on "Waiting for the browser sign-in…"

- **Cancel and retry:** click **Cancel** in the sign-in card at the top of the
  dialog. A sign-in nobody finishes expires after **5 minutes** and is cleaned
  up automatically; a brand-new app that never signed in is removed again.
- **Praxi runs on a server (or a paired host):** the sign-in page still opens in
  the browser of the computer you are at. The desktop app catches the answer on
  a temporary local port and passes it to the server over the paired, encrypted
  link, so there is nothing to copy. This needs the desktop app, and this
  computer must be confirmed as an administrator of the server (see
  [Remote host](remote-host.md)). If the app cannot catch the answer (for
  example you use Praxi in a plain browser tab), the card says so and shows one
  box: after you sign in the browser lands on a page that does not open and
  whose address starts with `http://127.0.0.1:`. Copy that full address and
  paste it into the box, then press **Finish sign-in**.
- **Browser window did not open:** the card says so and offers **Open the page
  again** and **Copy the link**; the sign-in keeps waiting.
- **GitLab returns a namespace error:** GitLab requires the MCP server to be
  enabled on your top-level group (`group[mcp_server_enabled]` in the group's
  General settings).

### Composio sign-in is stuck or the catalog will not refresh

- **Stuck on "Signing in":** the flow times out after **10 minutes**. Open the
  app page and choose **Cancel**, then **Connect** to start again.
- **"The app list could not be refreshed: showing the saved one":** you are
  offline, Composio is rate-limiting requests, or your Composio sign-in expired.
  Click **Try again** once you are back online. If sign-in expired, link again
  from the Composio account card.

### "The page for an app could not be prepared"

The connect sheet names the reason, and **Details** shows the exact text:

- **Only the Praxi desktop app on this computer can connect apps:** the owner
  confirmation is missing. Open Praxi on this computer and connect from there.
- **Composio does not offer this app:** pick another app from the list.
- **Praxi refused to connect it** (for example the account does not match the
  one saved before): **Disconnect** the app, then **Connect** again.
- **Composio is busy / could not be reached:** wait a minute, check the
  connection of the computer the bots run on, then **Try again**.
- **Composio no longer accepts the sign-in:** return to the account card and
  link Composio again, then retry from the app page.

### A local MCP server shows "Error" or "Quarantined"

Failed MCP servers display a specific hint directly under the row together with
a **Show details** toggle containing the **Error** message and **What the server
printed** (`stderr`):

![A local server that cannot start: the inline Install uv prompt and the expanded details](images/apps/apps-error.png)

- **Missing runtime (`Node.js`, `uv`, `pipx`, `docker`):** for **Node.js** and
  **uv**, Praxi shows an inline **Install** prompt (or you can install them from
  their official sites and restart Praxi-Bot), then press **Reconnect**.
- **Missing folder (`Filesystem`):** enter the full path to the single folder
  the bot may read and write and click **Save and connect**.
- **Server stopped while starting:** click **Show details** to read `stderr`
  (for example, Docker installed but the Docker daemon is not running), fix the
  underlying issue, and click **Reconnect**.
- **Timed out:** the first launch can take longer while `npx` or `uvx` downloads
  packages. Wait a moment and press **Reconnect**.
- **Missing or invalid secret / settings:** check your server configuration or
  re-add the server with a fresh token, then press **Reconnect**.
- **Quarantined:** the daemon paused the server after repeated failures. Fix the
  cause shown in **Show details**, then press **Reconnect** to clear the
  quarantine.
