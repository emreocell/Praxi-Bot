# Connecting apps (the Apps dialog)

The **Apps** dialog in the desktop app connects Praxi to external services
(such as Linear, Notion, Stripe, Atlassian, GitLab, or hundreds of apps through
Composio) and to local or custom [MCP servers](mcp-servers.md).

Open it from the **Apps** button at the bottom of the left rail (it shows up to
five logos of connected MCP servers when the rail is wide, or a grid icon when
narrow), or through the command palette (`Ctrl/Cmd+K`).

At the top of the dialog you have:

- **Search apps** — filters connected apps and the discovery list by name or
  description.
- **Advanced** — switches between the main Apps view and the advanced view for
  custom MCP servers, local developer servers, token-based services, and
  Composio account settings.

## Three ways apps connect

### 1. Direct OAuth apps

Remote HTTP MCP servers in the built-in catalog that need no API key appear
directly in **Discover** on the main screen (for example **Atlassian**,
**GitLab**, **Linear**, **Notion**, and **Stripe**).

1. Click **Connect** on the app tile.
2. Your system browser opens the provider's sign-in and consent page.
3. Finish signing in in the browser. Once the daemon receives the callback, the
   app moves into **Connected**.

Direct OAuth apps talk straight to the vendor's remote MCP server with no
intermediary. If a service is available both directly and through Composio,
Praxi shows only the direct tile so each product appears once.

### 2. Composio apps

The rest of the **Discover** grid lists third-party integrations available
through Composio. Filter them with the category chips (**All**,
**Communication**, **Social**, **Productivity**, **Dev**, **Finance**,
**Business**) or the search box, and use **Show more** to load additional pages.

Clicking **Connect** opens a small progress sheet anchored to the tile:

- **First time (Step 1 of 2):** Praxi opens the one-time free Composio sign-in
  in your browser. When that finishes, the same flow automatically continues to
  the app's own consent page (**Step 2 of 2**) without pressing **Connect**
  again.
- **Already linked (Step 1 of 1):** Praxi opens the app's consent page right
  away.

> **Privacy note:** For Composio-backed apps, tool inputs and results pass
> through Composio's servers. Praxi's permission engine still asks you before
> actions that change or send data (see
> [Chatting and approvals](chatting-and-approvals.md)).

If you prefer to use your own Composio project key instead of the browser login,
open **Advanced → Composio → Use my own project key**. The key is checked
immediately, stored in your OS keychain, and never shown again.

### 3. Advanced: MCP servers and token-based services

Click **Advanced** in the top bar for developer and self-managed connections:

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

## What "Connected" means

The **Connected** section at the top of the main screen shows only **settled**
remote connections:

- **Direct OAuth / remote HTTP MCP servers** after sign-in completes (or that
  are already saved in `mcp.toml`). While a brand-new OAuth sign-in is still in
  progress, the app stays as a **Signing in** tile in **Discover** with a
  **Cancel** button rather than appearing in **Connected**. If you cancel or do
  not finish within **10 minutes**, Praxi deletes the unfinished server entry
  automatically.
- **Composio apps** whose state is **Connected** (`active`) or **Sign-in
  expired** (`expired`). An unfinished Composio sign-in (**Signing in** or
  **Not finished**) stays in **Discover** with **Cancel** or **Retry** until
  Composio confirms the account is active.

Local stdio servers do not appear in the main **Connected** section; manage them
under **Advanced → Local developer servers**.

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

Hover or focus any row in **Connected** (or under **Advanced → Local developer
servers**) and click **Manage**. Rows that need attention (such as `Error`,
`Sign-in required`, `Sign-in expired`, or `Quarantined`) always keep **Manage**
visible.

### Direct OAuth and MCP servers

Inside **Manage** you can:

- **Choose which bots may use the app** under **Who can use this app** and click
  **Save**.
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

### Composio apps

Inside **Manage** on a Composio row you can:

- **Turn off** / **Turn on** (when **Connected**): temporarily disables or
  re-enables the app's tools without removing your OAuth link.
- **Reconnect** / **Retry** (when **Sign-in expired** or **Not finished**):
  reopens the consent flow in your browser.
- **Disconnect**: asks for confirmation and disconnects the app. If the remote
  side could not be removed automatically, Praxi tells you it was removed
  locally and reminds you to remove it in the Composio dashboard too.

To disconnect your Composio account itself (whether linked via browser login or
a project key), open **Advanced → Composio** and click **Sign out of Composio**.

## Per-bot access

Each MCP server (including direct OAuth apps) can be shared with all bots or
restricted to specific bots. Connected rows show up to four bot avatars (plus
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

- **Cancel and retry:** click **Cancel** next to **Signing in** (or in the
  **Manage** panel). Brand-new direct OAuth sign-ins also time out and clean up
  automatically after **10 minutes**.
- **Browser runs on another computer (or remote host):** if you completed
  sign-in in a browser that cannot reach Praxi's local callback port and the
  browser displays a `http://127.0.0.1:…/callback?…` error page, open **Manage**
  on the waiting server, paste that full page address into **Redirect address
  from the browser**, and click **Finish sign-in**.
- **Browser window did not open:** check your OS default browser and retry
  **Sign in** from **Manage**.
- **GitLab returns a namespace error:** GitLab requires the MCP server to be
  enabled on your top-level group (`group[mcp_server_enabled]` in the group's
  General settings).

### Composio sign-in is stuck or the catalog will not refresh

- **Stuck on "Signing in" or the progress sheet:** the flow times out after
  **10 minutes**. Click **Cancel** on the tile or sheet to clear the pending
  state, then click **Connect** (or **Retry**) to start again.
- **"The app list could not be refreshed: showing the saved one":** you are
  offline, Composio is rate-limiting requests, or your Composio sign-in expired.
  Click **Retry** once you are back online; if sign-in expired, open
  **Advanced → Composio → Sign out of Composio** and link again.

### A local MCP server shows "Error" or "Quarantined"

Failed MCP servers display a specific hint directly under the row together with
a **Show details** toggle containing the **Error** message and **What the server
printed** (`stderr`):

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
