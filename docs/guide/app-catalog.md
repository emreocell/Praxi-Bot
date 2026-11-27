# What you need to connect each app

Every app in the Apps list comes from a curated catalog. This page says, for each
one, **what it takes to sign in**, where the server runs and under which licence
its software is released. Nothing here is connected on its own: an app stays
off until you open **Apps**, pick it and connect it yourself, and every tool it
offers still asks before it changes anything (see
[Connecting apps](apps.md)).

## The four ways to sign in

| What you need | What it means |
| --- | --- |
| Nothing to set up | No account and no key: a local tool, or a public server. |
| Sign in with your account in the browser | The usual one-click path. Your browser opens the vendor's sign-in page; Praxi registers itself with the vendor automatically (dynamic client registration), so you create nothing first. |
| Sign in with an OAuth client you create yourself | The vendor needs an app you register in your own account first (Google: your own Cloud project). Praxi walks you through it; see [Google Workspace](google-workspace.md). |
| Paste an API key, access key or connection string | You create the key at the vendor and paste it once. It is stored in the secret store and never shown again. |

**Runs.** *Vendor's server* means the vendor runs the server and Praxi only calls it
over the internet, so nothing is installed and the vendor's own terms apply (the
licence column says *Hosted by the vendor*). *On this computer* means Praxi
starts a package on your machine, pinned to one exact version, and the licence
column is that package's licence.

## The catalog

<!-- BEGIN:app-catalog-table -->
| App | What you need | Runs | Server licence |
| --- | --- | --- | --- |
| [Airtable](https://airtable.com/developers/agents/mcp/getting-started) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Atlassian](https://www.atlassian.com/platform/remote-mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Brave Search](https://github.com/brave/brave-search-mcp-server) | Paste an API key, access key or connection string | On this computer | MIT |
| [Calendly](https://developer.calendly.com/docs/mcp/calendly-mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [ClickUp](https://developer.clickup.com/docs/connect-an-ai-assistant-to-clickups-mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Cloudflare](https://developers.cloudflare.com/agents/model-context-protocol/cloudflare/servers-for-cloudflare/) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Cloudflare Docs](https://github.com/cloudflare/mcp-server-cloudflare/tree/main/apps/docs-ai-search) | Nothing to set up | Vendor's server | Hosted by the vendor |
| [Context7](https://github.com/upstash/context7) | Nothing to set up | On this computer | MIT |
| [DeepWiki](https://docs.devin.ai/work-with-devin/deepwiki-mcp) | Nothing to set up | Vendor's server | Hosted by the vendor |
| [Dropbox](https://help.dropbox.com/integrations/connect-dropbox-mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Exa](https://github.com/exa-labs/exa-mcp-server) | Paste an API key, access key or connection string | On this computer | MIT |
| [Fetch](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch) | Nothing to set up | On this computer | MIT |
| [Figma](https://developers.figma.com/docs/figma-mcp-server/) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) | Nothing to set up | On this computer | Apache-2.0 AND MIT |
| [Firecrawl](https://github.com/firecrawl/firecrawl-mcp-server) | Paste an API key, access key or connection string | On this computer | MIT |
| [GitHub (access token)](https://github.com/github/github-mcp-server) | Paste an API key, access key or connection string | Vendor's server | Hosted by the vendor |
| [GitLab](https://docs.gitlab.com/user/model_context_protocol/mcp_server/) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Gmail (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Sign in with an OAuth client you create yourself | Vendor's server | Hosted by the vendor |
| [Google Calendar (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Sign in with an OAuth client you create yourself | Vendor's server | Hosted by the vendor |
| [Google Drive (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Sign in with an OAuth client you create yourself | Vendor's server | Hosted by the vendor |
| [Home Assistant](https://github.com/voska/hass-mcp) | Paste an API key, access key or connection string | On this computer | MIT |
| [Intercom](https://developers.intercom.com/docs/guides/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Linear](https://linear.app/docs/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Microsoft Learn](https://github.com/microsoftdocs/mcp) | Nothing to set up | Vendor's server | Hosted by the vendor |
| [Miro](https://developers.miro.com/docs/miro-mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [monday.com](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [MongoDB](https://github.com/mongodb-js/mongodb-mcp-server) | Paste an API key, access key or connection string | On this computer | Apache-2.0 |
| [Neon](https://neon.com/docs/ai/neon-mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Netlify](https://github.com/netlify/netlify-mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Notion](https://github.com/makenotion/notion-mcp-server) | Paste an API key, access key or connection string | On this computer | MIT |
| [Notion](https://developers.notion.com/docs/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Obsidian](https://github.com/MarkusPfundstein/mcp-obsidian) | Paste an API key, access key or connection string | On this computer | MIT |
| [PayPal](https://developer.paypal.com/ai-tools/mcp-server) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Playwright](https://github.com/microsoft/playwright-mcp) | Nothing to set up | On this computer | Apache-2.0 |
| [PostgreSQL](https://github.com/crystaldba/postgres-mcp) | Paste an API key, access key or connection string | On this computer | MIT |
| [PostHog](https://posthog.com/docs/model-context-protocol) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Sentry](https://github.com/getsentry/sentry-mcp) | Paste an API key, access key or connection string | On this computer | FSL-1.1-ALv2 |
| [Slack](https://github.com/modelcontextprotocol/servers-archived/tree/main/src/slack) | Paste an API key, access key or connection string | On this computer | MIT |
| [SQLite](https://github.com/modelcontextprotocol/servers-archived/tree/main/src/sqlite) | Nothing to set up | On this computer | MIT |
| [Square](https://developer.squareup.com/docs/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Stripe](https://docs.stripe.com/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Supabase](https://supabase.com/docs/guides/ai-tools/mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Tavily](https://github.com/tavily-ai/tavily-mcp) | Paste an API key, access key or connection string | On this computer | MIT |
| [Todoist](https://github.com/todoist/todoist-mcp) | Paste an API key, access key or connection string | On this computer | MIT |
| [Vercel](https://vercel.com/docs/agent-resources/vercel-mcp) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
| [Webflow](https://developers.webflow.com/mcp/reference/overview) | Sign in with your account in the browser | Vendor's server | Hosted by the vendor |
<!-- END:app-catalog-table -->

The table is generated from `crates/mcp-bridge/catalog.json`; a check fails when
the two differ. Only official servers (from the vendor, or listed in the MCP
registry) are added. Some vendors' servers only accept a client you register
with them first (when this was written: Asana, HubSpot and Box); those are not in
the list until a sign-in without setup is possible.

## Related

- [Connecting apps](apps.md): connecting, managing and signing out.
- [MCP servers](mcp-servers.md): adding a server by hand.
- [Keep your bot safe](keep-your-bot-safe.md): what the permission levels mean.
