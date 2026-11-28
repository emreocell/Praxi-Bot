# Connect a REST API (OpenAPI)

Any web service that publishes an **OpenAPI 3.0 or 3.1** document can become a set of
tools for your bots, without a plugin or an MCP server. You give Praxi the document, the
address of the API and (if it needs one) a key. Praxi lists the operations it finds and
you turn on the ones a bot may use.

Open **Apps** and choose **REST APIs** in the top bar.

## Connect an API

1. **Name:** anything you like. It becomes the prefix of the tools (`api_<name>_<operation>`).
2. **Where is the document?** Give a **web address** (for example
   `https://api.example.com/openapi.json`) or choose a **file** (JSON or YAML) from your
   computer. A web address can be refreshed later with **Update from the address**; a file
   cannot.
3. **API address:** requests go only to this address. You can leave it empty only when you
   gave a web address and the document lists a server on that same site; that server is then
   used. For a file, or a document that names a server on another site, you must enter it:
   a document cannot decide where your key is sent.
4. **Sign-in:** choose what the API expects:

   | Choice | What Praxi sends |
   | --- | --- |
   | None | nothing |
   | Access key (Bearer) | `Authorization: Bearer <key>` |
   | API key in a header | the key in the header you name (for example `X-Api-Key`) |
   | API key in the address | the key as the query parameter you name (prefer a header when the API offers both: addresses end up in the service's own logs) |
   | User name and password | HTTP basic authentication |

5. Choose **Connect API**.

The key or password is saved in your secret store (the OS keychain, or the encrypted
file on a server) under `openapi/<id>/credential`. It is never shown again and is never part
of what a bot reads: not in the tool list, not in a request the bot sees, not in the result,
not in logs. The document itself is kept in `<data dir>/openapi-specs/`, and the settings (no
secrets) in `<data dir>/openapi-apps.toml`.

## Turn on operations, per bot

Open the connected API. **Nothing is on at first.** Pick a bot (or **All bots**) under
**Show access for**, then tick the operations that bot may use. Use the search box to find
one in a long document.

- An operation turned on for **All bots** is open to every bot, including bots you create
  later.
- An operation turned on for one bot is available to that bot only. The other bots do not
  even see the tool.
- Turning a bot off for an operation that was open to every bot lists all the other bots
  instead. An operation nobody may use any more is switched off; it is never opened up.
- At most 50 operations can be on at once, because every one is a tool the model has to read.
- A bot you delete is removed from these lists.

Changes apply at once; no restart is needed.

## What asks for approval

The risk of an operation comes from its HTTP method:

| Method | Class |
| --- | --- |
| `GET`, `HEAD`, `OPTIONS` | read |
| `POST`, `PUT`, `PATCH` | write |
| `DELETE` | destructive |

Whether a call asks is then decided by the bot's permission level, exactly as for its other
tools: a read is treated as a read, a change as a write, a delete as destructive. At **Full
access** none of them ask.

**Money-like operations always ask, at every level, including Full access.** An operation is
money-like when its path or its `operationId` contains a word such as *pay*, *payment*,
*payout*, *charge*, *checkout*, *purchase*, *buy*, *subscription*, *transfer*, *refund*,
*withdraw*, *invoice* or *billing*. The operation list marks it **Always asks**. This
applies to reads as well (listing invoices asks too), because the match is by name only and it
is safer to ask once too often than to pay once too often. Money-like tools belong to the
**Money** permission category.

Because the tools contact an outside service and return text written by someone else, the
usual protections for untrusted content apply as well: after a bot has read a web page or
an e-mail in a run, a call to the API asks first.

## What Praxi guards

- **One host.** Requests go only to the API address you gave. Path values a bot supplies are
  encoded and cannot change the host or leave the base path (`..` is refused).
- **No redirects.** An API that answers with a redirect is not followed, not even to the same
  host, so a redirect cannot send your key anywhere.
- **Only your address.** The document's own `servers` are ignored when you give an address.
  Links to other documents (`$ref` to a web address) are never fetched.
- **Header values.** A line break in a value a bot supplies for a header is refused.
- **No private addresses.** Addresses on your own machine or network (`localhost`,
  `192.168.x.x`, `10.x.x.x`, link-local and cloud metadata addresses) are refused for the
  API and for downloading the document, also when a name resolves to one.
- **No proxy.** The environment's proxy settings are not used for these requests.
- **Size caps.** The document is limited to 8 MiB, an operation whose schemas nest `$ref`s more
  than 10 deep or expand to more than 1 MiB is left out and counted under the API, a response is cut at 256 KiB (or less)
  and a request times out after 30 seconds. A cut response is marked as truncated.
- **Secrets stay out.** The key is read only when a call is made and masked in every result,
  error and log, also when it travels in the address and the API echoes it back. A header or
  query parameter that carries the key is hidden from the tool and cannot be set by a bot.
- **Money asks.** Besides the path and the operation id, the tags, summary and description are
  read for payment words, and a `POST` to a one-door endpoint such as `/graphql` or `/rpc`
  (where the path says nothing) always asks too.

## Limits and things to know

- Only local `$ref`s in the document are resolved. Operations that use file uploads
  (`multipart/form-data`), or whose parameters cannot be resolved, are left out and counted
  under the API.
- Deprecated operations are not listed.
- Each tool is named `api_<id>_<operationId>` (or `api_<id>_<method>_<path>` when the document
  gives no `operationId`).
- All requests of an API use one sign-in. For OAuth 2 flows, paste the access key you got as a
  Bearer access key; Praxi does not refresh it.
- The API and the document must be on a public address. A service on your own network needs
  a different route for now.
- If a document changes, choose **Update from the address**. Operations that still exist stay
  on; operations that vanished are switched off.

## HTTP API

The same functions are available to the desktop app and to scripts that hold the owner key,
under `/v1/openapi-apps` (see [`docs/api/openapi.json`](../api/openapi.json)):
`GET`/`POST /v1/openapi-apps`, `GET`/`PUT`/`DELETE /v1/openapi-apps/{id}` and
`POST /v1/openapi-apps/{id}/refresh`. These routes are owner-only: the plain API token is not
enough, and a paired phone or other device is refused. The credential is write-only.

See also: [Connecting apps](apps.md), [MCP servers](mcp-servers.md),
[Keep your bot safe](keep-your-bot-safe.md).
