# Web tools: search and read pages

Every bot can look things up on the web without opening a browser. The web
tools are on by default and need no setup. They are the fast layer; the
[browser tools](bots-and-profiles.md) stay for pages that need JavaScript, a
login or clicking around.

| Tool | What it does |
| --- | --- |
| `web_search` | Search the web; returns titles, URLs and snippets. |
| `web_extract` | Fetch a URL over HTTP and return the page as readable Markdown (main content, tables, links). Use it to read a page. |
| `web_feed` | Fetch and parse an RSS, RDF or Atom feed. |
| `web_sitemap` | Fetch and parse a `sitemap.xml`, sitemap index or text sitemap. |
| `web_metadata` | Page title, description, canonical URL, OpenGraph, Twitter card, feeds and JSON-LD. |

An optional [web scraping add-on](web-scraping.md) adds `web_scrape` for cleaner
article text, link lists and small crawls of one site; it is not installed by default.

A typical question that needs a current fact goes `web_search`, then
`web_extract` on the best result. The browser is only used when the page itself
needs a real browser.

Bots made from the "General assistant" and "Browser researcher" presets can use
these tools. For a bot with its own `[tools] allow` list, add `"web_*"` (see
[Bots and profiles](bots-and-profiles.md)).

## Search backend

Without configuration, search uses a keyless fallback chain (`duckduckgo` → `duckduckgo_lite` → `bing`), requiring no account or API key. If an upstream returns a bot challenge or rate limit, that backend enters a short cooldown and `web_search` falls through to the next configured backend.

**On a server this chain usually does not work.** DuckDuckGo and Bing treat the addresses of data centres and cloud machines as robots and answer with a robot check instead of results. Praxi recognises these pages (a Cloudflare "Just a moment", a captcha, DuckDuckGo's "bots use DuckDuckGo too", an HTTP 403, 429 or 202) and says so in plain words: "the free search engines refused this computer as a robot", with the fix. A computer at home mostly works, a server mostly does not. The fix is a search provider that is meant to be used by programs; see [Search on a server](#search-on-a-server).

You can configure backends, priority order, API keys, and run a live probe either in **Settings → Web search** in the desktop app or in `<data_dir>/web.toml`. Backends are tried in configured priority order and the first that returns results wins (default order):

1. a [SearXNG](https://docs.searxng.org/) instance you run (`searxng_url`),
2. Brave Search API (`brave_key`),
3. Tavily Search API (`tavily_key`),
4. DuckDuckGo HTML (`duckduckgo`, default `true`),
5. DuckDuckGo Lite (`duckduckgo_lite`, default `true`),
6. Bing HTML (`bing`, default `true`).

```toml
[search]
searxng_url = "http://127.0.0.1:8080"   # your own instance; it may be local/private
brave_key = "web/brave"                 # NAME of a secret, never the key itself
tavily_key = "web/tavily"               # NAME of a secret, never the key itself
duckduckgo = true                       # keyless fallback (default true)
duckduckgo_lite = true                  # keyless fallback (default true)
bing = true                             # keyless fallback (default true)
order = ["searxng", "brave", "tavily", "duckduckgo", "duckduckgo_lite", "bing"]

[http]
timeout_secs = 20            # whole request (search backends are capped at 8s each)
connect_timeout_secs = 8
max_bytes = 5242880          # response body cap (at most 64 MiB)
max_redirects = 5            # at most 10
```

Every key is optional. Store Brave or Tavily keys in the secret store (or enter them in Settings → Web search, which stores them write-only), then reference the secret name in `web.toml`:

```sh
bot secrets set web/brave
bot secrets set web/tavily
```

A broken `web.toml` (a typo, a bad value) never stops the daemon: it logs a
warning and uses the defaults. Unknown keys count as a typo, so mistakes are
not silently ignored.

Large pages and feeds are cut to a size the model can read, and the full text
is saved under `<data_dir>/spill/`. Those files are not cleaned up
automatically yet; delete old ones by hand if they pile up.

## Search on a server

Pick one search provider, once. Any of these makes `web_search` work on a server:

| Provider | What you need | Notes |
| --- | --- | --- |
| Brave Search API | an API key from your Brave Search API account | Hosted. Has a usage limit that depends on your plan. |
| Tavily | an API key from your Tavily account | Hosted. Built for AI agents. Has a usage limit that depends on your plan. |
| SearXNG | the address of a SearXNG server | Runs on your own server, no account, no per-search cost. The one-command install is below. |

**In the app:** open **Settings → Web search**, choose the provider, paste the key (or the SearXNG address) and press **Save and test**. The test runs a real search through that provider and tells you if it answered. Keys are stored write-only in the secret store and are never shown again.

**Where you will see the warning:** the Web search page shows a notice when the computer has no screen and no provider is set, `bot doctor` prints a warning on the `web search` line, and the daemon logs the same warning at start. `bot doctor --online` and the **Search** button run a real search; when every free engine refuses, they say "refused this computer as a robot" instead of a bare HTTP code.

### Install SearXNG on the server (one command)

You need Docker on the server. On the server, from a checkout of this repository:

```sh
sudo bash scripts/install-searxng.sh --allow-latest
```

Without `--allow-latest` (or a pinned `--image`, below) the script stops before it downloads anything: the container starts as root, so it will not run an image that can change under you unless you say so. It writes `/opt/praxi/searxng/settings.yml` (JSON answers on, a random secret key), starts a container named `praxi-searxng` that listens on `127.0.0.1:8080` only and restarts with the server, asks it one real search and prints the address to give to Praxi: `http://127.0.0.1:8080`. Paste that into **Settings → Web search → SearXNG**, or put it in `web.toml` (`searxng_url`) and restart Praxi.

- `--port <n>` picks another port, `--dir <path>` another settings folder, `--image <ref>` the image to run. Only the official `docker.io/searxng/searxng` is accepted, pinned by digest (`--image docker.io/searxng/searxng@sha256:<64 hex digits>`; pick one you trust on the image's Docker Hub tags page). `--allow-latest` is the alternative: it accepts a moving tag (`latest`, or one you name with `--image docker.io/searxng/searxng:<tag>`), pulls it once, prints the digest it got and runs that exact digest, with the command to pin it next time.
- Run it again at any time: it replaces the container and keeps your `settings.yml`.
- `sudo bash scripts/install-searxng.sh --uninstall` removes the container and keeps the settings folder.
- It does not install Docker. If `docker` is missing the script stops and says so.
- SearXNG forwards your queries to the engines it uses, from the server's address. If those engines also refuse your server, edit the list of engines in `settings.yml` (see the [SearXNG documentation](https://docs.searxng.org/)).
- Praxi may only reach a private address (loopback or your LAN) for the one SearXNG address you configure yourself, over `http` or `https` with no user name or password in it. Every redirect that server answers with is checked again as if it were any other address: a redirect to another private address, to a different port, to cloud metadata or link-local space, or to Praxi's own API port is refused.

## Privacy

Search queries go to the configured backend that handles them (your SearXNG instance, Brave, Tavily, DuckDuckGo, or Bing), and fetched URLs go to the sites they point at, from your machine's IP address. Page fetches (`web_extract`, `web_feed`, `web_sitemap`, `web_metadata`) carry a `User-Agent: Praxi-Bot/<version> (web-tools)` header, while HTML search requests use standard browser `User-Agent` / `Accept` headers so search engines do not serve bot challenges. If query privacy matters to you, point `searxng_url` at an instance you control or remove the `web_*` tools from a bot's `allow` list.

## Safety

- **Network limits.** The tools only fetch `http` and `https` URLs. Loopback
  (`127.0.0.1`, `localhost`), private networks, link-local and cloud metadata
  addresses (`169.254.169.254`), and the daemon's own API are refused. The check
  is repeated on every redirect and on the addresses a hostname resolves to, so a
  public page cannot bounce the bot into your network. Only the SearXNG address
  you configure yourself (that host and port, never link-local or metadata
  space) is allowed to be private; its redirects get the same checks.
- **Untrusted content.** Everything a page says is data. Each result starts with
  an "untrusted external content" notice, and a bot must not treat instructions
  inside a page as coming from you.
- **Approvals.** The tools are split by where they send data:
  - `web_search` only talks to the search provider you configured, so it runs
    without asking, like other read-only tools, until the run has read private
    data. After untrusted content only (a page, a feed), a later search still
    does not ask. A standing grant does not waive a fetch once the run is
    tainted, unless the bot's "allow sending data after reads" setting is on.
  - `web_extract`, `web_feed`, `web_sitemap` and `web_metadata` contact any
    address the model picks, so they **ask for approval on every call under the
    default policy**, exactly like `browser_open`. After the run has read
    untrusted or private content they ask even under a grant or the `full`
    preset, unless that bot setting is on.
  - `web_scrape` (only where the optional add-on is installed) is treated like
    `web_extract`: it contacts any address the model picks, so it asks on every
    call under the default policy and after a read.
  - All of them belong to the `browser` permission category: if you set `browser`
    to `deny` for a bot, they are all denied.
  - In the **Full** preset, a run you started yourself reads the web without a
    card, `web_scrape` included: it follows the same rule as `web_extract` and
    `browser_open`. It still asks when the address or query copies the private
    data of the run, carries a key or a card number, or points at this computer or
    your network. Money stays always-ask. See
    [Chatting and approvals](chatting-and-approvals.md).

### Letting a bot fetch without asking

If you trust a bot enough, approve one fetch with the "always" choice (the
`always` scope in [Chatting and approvals](chatting-and-approvals.md)). That
remembers `web_extract` (or the tool you approved) for that bot only, and later
calls run without a prompt while the run has not read anything. Once a page or
a private read is still in the conversation the model sees, a later fetch asks
again. Grant each fetch tool separately; `always_category` would also cover
every other `browser` tool, so prefer the per-tool choice. You can review and
remove grants as described in that guide. The per-bot "allow sending data after
reads" setting is the only way to skip the ask after a read.

## Known risk

A fetch you have granted, or a `full` bot, can still send data out until the
run has read something. After that read, the egress gate asks on later turns
too, while the read is still in the conversation (see
[Security model](security.md)), unless you turned on the per-bot setting. The
URL and the query are shown in full on the card. URLs are capped at 2,048
characters and search queries at 400, local and private addresses are refused,
and results are labelled untrusted. Do not turn the setting on for a bot that
handles secrets.

## Check that it works

```sh
bot doctor --online
```

The `web search` line shows which backends are configured and whether a live
search answered. A search that does not answer is a warning, not an error.

## Browser use: what to expect

The bot's own browser (`browser_open`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_tabs`, ...) is a managed Chrome or Edge that starts on the first call.

- **Where it runs.** On your computer it opens a real window so you can watch; on an always-on server without a display it is headless and you watch it through the live view of the chat.
- **Slow pages.** A page that has not finished loading after 20 seconds is no longer a failure: the bot gets what is on screen with a note that the page was still loading, and carries on instead of waiting blindly.
- **Downloads are blocked.** A page can never put a file on your disk through the bot's browser; the download is cancelled.
- **No browser tools?** The bot is told exactly why in its instructions (the computer is switched off, it is not reachable, or no Chrome or Edge is installed) and will say so instead of guessing. Install Chrome or Edge, or change "Runs on" in the chat bar.
- **Working method the bot is taught.** Open the page (the result already lists clickable elements with refs), act on those refs, dismiss cookie banners first, use the tab list for popups, and take a screenshot only for questions about how a page looks.
- Approvals are unchanged: public http(s) pages need no extra approval; `file:` and local addresses ask.
