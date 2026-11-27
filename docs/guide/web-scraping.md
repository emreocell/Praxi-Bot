# Web scraping add-on (optional)

The built-in [web tools](web.md) read most pages already. The **web scraping
add-on** is a small extra program you can add to a server when you want
cleaner article text, the list of links on a page, or a short crawl (up to 10
pages of one site). It adds one tool, `web_scrape`. Bots are told to try
`web_extract` first and to use `web_scrape` only when that is not enough.

It is **not** for pages you have to log in to, and it does not run JavaScript.
It never logs in, never solves CAPTCHAs, never hides who it is and never gets
past a paywall. If a site says no, the bot is told to stop and say so.

## Add or remove it

Only you can do this, from your own computer, over your SSH login: the server's
Praxi user has no administrator rights, so a bot can never install it.

1. Open the **computer picker** at the top → **Your servers**, pick the server and press **Tool packs**.
2. Tick **Web scraping**. It needs Python, which comes with **Developer tools**
   (add that pack first if the list says Python is needed).
3. Press **Review**: you see exactly what would change before anything
   happens. Confirm to install; the server downloads about 40 MB.
4. To remove it, use **Remove** on the same row. Removing deletes the add-on,
   its service and its key. Python and your other tools stay.

Installing again is safe: it does the same thing and repairs a broken install.
The tool appears for bots the next time Praxi on that server starts (after a
restart or an update of Praxi there).

**There is no per-bot switch.** Once the add-on is installed on a server, every bot
that runs on that server gets `web_scrape`, just like the other web tools. What
it may do is still limited by the usual approvals: reading a page is a
Browser-level action, and a bot that read a page cannot send private data out
without asking you. To turn it off for all bots, use **Remove**.

### What the install does

- Creates `/opt/praxi/packs/scrape/venv` and installs Trafilatura and its
  dependencies from `requirements.lock`. Every file is checked against a pinned
  SHA-256 hash and only ready-made wheels are used, so nothing is compiled or
  run from the internet.
- Installs the program `praxi_scrape.py` (a few hundred lines, in this repository
  under `packs/scrape/`) and a systemd service `praxi-scrape`. The Praxi app
  checks the SHA-256 of all three files on the server before it installs them.
- Creates a random key at `/etc/praxi/scrape.token`, readable only by root and
  the Praxi service user. The add-on answers only requests that carry it.
- The service runs as its own throwaway user with `NoNewPrivileges`, a read-only
  system, no access to home folders or to Praxi's own data folder, memory and
  task limits, and it is not allowed to open connections to private or
  cloud-metadata addresses.
- The server's own DNS addresses are allowed through a small drop-in file that
  is written from `/etc/resolv.conf` **at install time**. If the server's DNS
  settings change later (for example a new DHCP lease), press **Tools** and
  install the add-on again to refresh it.

## What the add-on does and does not do

- Listens on `127.0.0.1` only (port 47811). Nothing outside the computer can
  reach it, and other programs on it need the key.
- Reads public web pages only. It refuses private, loopback, link-local and
  cloud-metadata addresses, checks every redirect again, and stops at 5
  redirects, 5 MB and 20 seconds per page. The time limit is a hard one: a site
  that sends its data a few bytes at a time is cut off at the limit, and a
  stuck page can never block the add-on for the next request. Only HTML, XML and
  text are read; no cookies.
- Respects `robots.txt`, including `Crawl-delay`, and says who it is:
  `PraxiBot/1 (+https://github.com/emreocell/Praxi-Bot)`.
- Is polite: at most 1 request per second and 30 per minute to one site (the
  `robots.txt` request counts too), and 2 pages at a time overall.
- Text it brings back is **untrusted page content**, exactly like
  `web_extract`: it is data for the bot to read, never instructions. A bot that
  read a page cannot send private data out without asking you first.

## Licences

| Part | Licence |
| --- | --- |
| Praxi's own `praxi_scrape.py` | Apache-2.0 |
| [Trafilatura](https://github.com/adbar/trafilatura) 2.4.0 (page text) | Apache-2.0 |
| Its dependencies (courlan, htmldate, lxml, dateparser, urllib3, ...) | Apache-2.0, BSD, MIT; `certifi` is MPL-2.0 (unchanged) |

The tool-pack dialog shows the licence and a source link for every pack.
The full list with versions and checksums is in `THIRD_PARTY_NOTICES.md` and
`packs/scrape/requirements.lock`.

A separate program under a copyleft licence (for example AGPL-3.0) is never
bundled. If one is offered as a pack later, it is downloaded from its author,
and you must read and confirm its licence notice before it installs.

## Personal data

Pages can contain personal data. Under KVKK and GDPR you are responsible for
what you collect, why, and how long you keep it. Read only what you need, do
not store it longer than needed, and check a site's terms before you crawl it.
This is general information, not legal advice.

## The local interface (for developers)

`POST http://127.0.0.1:47811/v1/extract` with the header `X-Praxi-Token` and a
JSON body `{"url", "mode": "article|markdown|links", "max_chars", "timeout_ms",
"max_pages"}` returns `{"status", "final_url", "title", "markdown", "links",
"truncated", "robots": "allowed|blocked|unknown", "fetched_at", "pages",
"error"?}`. `GET /health` returns `{"ok": true}`. Another reader (for example
a browser-based one) can be added later behind the same contract.
